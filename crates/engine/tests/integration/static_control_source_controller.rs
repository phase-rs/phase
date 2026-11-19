//! CR 109.5 + CR 613.1b + CR 613.8a-c: "you" in a static control effect ("You
//! control enchanted creature.") is the current controller of the Aura it is
//! on. When that Aura itself changes controller in layer 2, the enchanted
//! permanent follows the Aura's new controller (phase-rs/phase#10947).
//!
//! Every row drives the production cast / priority pipeline with cards built
//! from their verbatim Oracle text (Scryfall).

use engine::game::layers::evaluate_layers;
use engine::game::perf_counters;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{TargetFilter, TypedFilter};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P2: PlayerId = PlayerId(2);

const KHARN: &str = "Berzerker \u{2014} Kh\u{e2}rn the Betrayer attacks or blocks each combat if \
    able.\nSigil of Corruption \u{2014} When you lose control of Kh\u{e2}rn the Betrayer, draw two \
    cards.\nThe Betrayer \u{2014} If damage would be dealt to Kh\u{e2}rn the Betrayer, prevent that \
    damage and an opponent of your choice gains control of it.";
const CONTROL_MAGIC: &str = "Enchant creature\nYou control enchanted creature.";
const CONFISCATE: &str = "Enchant permanent\nYou control enchanted permanent.";
const WORD_OF_SEIZING: &str = "Split second (As long as this spell is on the stack, players can't \
    cast spells or activate abilities that aren't mana abilities.)\nUntap target permanent and \
    gain control of it until end of turn. It gains haste until end of turn.";
const ACT_OF_TREASON: &str = "Gain control of target creature until end of turn. Untap that \
    creature. It gains haste until end of turn. (It can attack and {T} this turn.)";
const NATURALIZE: &str = "Destroy target artifact or enchantment.";

const LIBRARY: [&str; 12] = [
    "Lib 1", "Lib 2", "Lib 3", "Lib 4", "Lib 5", "Lib 6", "Lib 7", "Lib 8", "Lib 9", "Lib 10",
    "Lib 11", "Lib 12",
];

fn give_priority(runner: &mut GameRunner, player: PlayerId) {
    let state = runner.state_mut();
    state.active_player = player;
    state.priority_player = player;
    state.waiting_for = WaitingFor::Priority { player };
}

fn controller(runner: &GameRunner, id: ObjectId) -> PlayerId {
    runner.state().objects[&id].controller
}

fn library_size(runner: &GameRunner, player: PlayerId) -> usize {
    runner.state().players[player.0 as usize].library.len()
}

/// Every `ControllerChanged` for `object` in `events`, as (old, new).
fn changes_of(events: &[GameEvent], object: ObjectId) -> Vec<(PlayerId, PlayerId)> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::ControllerChanged {
                object_id,
                old_controller,
                new_controller,
                ..
            } if *object_id == object => Some((*old_controller, *new_controller)),
            _ => None,
        })
        .collect()
}

/// Khârn as printed: {3}{R}, Legendary, 5/1.
fn add_kharn(scenario: &mut GameScenario, owner: PlayerId) -> ObjectId {
    scenario
        .add_creature_from_oracle(owner, "Kh\u{e2}rn the Betrayer", 5, 1, KHARN)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 3,
        })
        .as_legendary()
        .id()
}

/// A zero-cost Aura in `player`'s hand. The Enchant keyword is installed after
/// `build` by [`make_aura`].
fn add_aura(scenario: &mut GameScenario, player: PlayerId, name: &str, text: &str) -> ObjectId {
    scenario
        .add_artifact_to_hand_from_oracle(player, name, text)
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(ManaCost::zero())
        .id()
}

fn make_aura(runner: &mut GameRunner, aura: ObjectId, enchant: TypedFilter) {
    let obj = runner.state_mut().objects.get_mut(&aura).unwrap();
    obj.card_types.core_types = vec![CoreType::Enchantment];
    obj.base_card_types = obj.card_types.clone();
    let enchant = Keyword::Enchant(TargetFilter::Typed(enchant));
    obj.keywords.push(enchant.clone());
    obj.base_keywords.push(enchant);
}

fn add_word_of_seizing(scenario: &mut GameScenario, player: PlayerId) -> ObjectId {
    scenario
        .add_spell_to_hand(player, "Word of Seizing", true)
        .from_oracle_text_with_keywords(&["Split second"], WORD_OF_SEIZING)
        .with_mana_cost(ManaCost::zero())
        .id()
}

/// A zero-cost instant or sorcery (`kind`) in `player`'s hand.
fn add_free_spell(
    scenario: &mut GameScenario,
    player: PlayerId,
    name: &str,
    kind: CoreType,
    text: &str,
) -> ObjectId {
    assert!(
        matches!(kind, CoreType::Instant | CoreType::Sorcery),
        "a spell kind: {kind:?}"
    );
    scenario
        .add_spell_to_hand_from_oracle(player, name, kind == CoreType::Instant, text)
        .with_mana_cost(ManaCost::zero())
        .id()
}

fn act(runner: &mut GameRunner, action: GameAction, events: &mut Vec<GameEvent>) {
    events.extend(
        runner
            .act(action)
            .expect("the public action must be accepted")
            .events,
    );
}

/// Passes priority (and declares no attackers or blockers) until `stop` holds.
/// Any other prompt fails the test, so an unexpected choice cannot hide.
fn drive_until(
    runner: &mut GameRunner,
    events: &mut Vec<GameEvent>,
    what: &str,
    stop: impl Fn(&GameState) -> bool,
) {
    for _ in 0..400 {
        if stop(runner.state()) {
            return;
        }
        let action = match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            },
            WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                assignments: vec![],
            },
            other => panic!("unexpected prompt while driving to {what}: {other:?}"),
        };
        act(runner, action, events);
    }
    panic!("never reached {what}");
}

fn settle(runner: &mut GameRunner, events: &mut Vec<GameEvent>) {
    drive_until(runner, events, "an empty stack", |state| {
        state.stack.is_empty() && matches!(state.waiting_for, WaitingFor::Priority { .. })
    });
}

/// Passes until this turn's cleanup step, or the next turn when cleanup
/// grants no priority.
fn drive_to_end_of_turn(runner: &mut GameRunner, events: &mut Vec<GameEvent>) {
    let turn = runner.state().turn_number;
    drive_until(runner, events, "end of turn", |state| {
        state.phase == Phase::Cleanup || state.turn_number != turn
    });
}

/// JSON save and load of the whole authoritative state.
fn round_trip(runner: &mut GameRunner) {
    runner.state_mut().capture_rng_word_pos();
    let saved = serde_json::to_string(runner.state()).expect("state serializes");
    let mut restored: GameState = serde_json::from_str(&saved).expect("state restores");
    restored.rehydrate_rng();
    *runner.state_mut() = restored;
}

fn board(players: u8) -> GameScenario {
    let mut scenario = if players == 2 {
        GameScenario::new()
    } else {
        GameScenario::new_n_player(players, 42)
    };
    scenario.at_phase(Phase::PostCombatMain);
    for player in 0..players {
        scenario.with_library_top(PlayerId(player), &LIBRARY);
    }
    scenario
}

/// `caster`'s Aura (already made an Aura) resolves on `target`.
fn cast_aura(
    runner: &mut GameRunner,
    caster: PlayerId,
    aura: ObjectId,
    target: ObjectId,
) -> Vec<GameEvent> {
    give_priority(runner, caster);
    let outcome = runner.cast(aura).target_object(target).resolve();
    let mut events = outcome.events().to_vec();
    settle(runner, &mut events);
    events
}

struct WordBoard {
    runner: GameRunner,
    kharn: ObjectId,
    magic: ObjectId,
    bear: ObjectId,
    naturalize: ObjectId,
    /// Events of the action in which Word of Seizing resolved, then settled.
    word_events: Vec<GameEvent>,
    /// Control-dependency previews counted while Word of Seizing resolved.
    word_previews: u64,
    p1_library_before_word: usize,
}

/// P1's Control Magic takes P0's Khârn; then P0's Word of Seizing takes the
/// Control Magic. P0 also holds a textless {0} creature, and P1 a Naturalize.
fn word_board() -> WordBoard {
    let mut scenario = board(2);
    let kharn = add_kharn(&mut scenario, P0);
    let magic = add_aura(&mut scenario, P1, "Control Magic", CONTROL_MAGIC);
    let word = add_word_of_seizing(&mut scenario, P0);
    let bear = scenario
        .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
        .with_mana_cost(ManaCost::zero())
        .id();
    let naturalize = add_free_spell(
        &mut scenario,
        P1,
        "Naturalize",
        CoreType::Instant,
        NATURALIZE,
    );
    let mut runner = scenario.build();
    make_aura(&mut runner, magic, TypedFilter::creature());
    cast_aura(&mut runner, P1, magic, kharn);
    assert_eq!(
        controller(&runner, kharn),
        P1,
        "REACH: Control Magic took Khârn"
    );

    give_priority(&mut runner, P0);
    let p1_library_before_word = library_size(&runner, P1);
    perf_counters::reset();
    let outcome = runner.cast(word).target_object(magic).resolve();
    let word_previews = perf_counters::control_dependency_snapshot().previews;
    let mut word_events = outcome.events().to_vec();
    assert_eq!(
        controller(&runner, magic),
        P0,
        "REACH: P0 controls Control Magic"
    );
    settle(&mut runner, &mut word_events);
    WordBoard {
        runner,
        kharn,
        magic,
        bear,
        naturalize,
        word_events,
        word_previews,
        p1_library_before_word,
    }
}

/// R1: Word of Seizing (a resolved, until-end-of-turn control effect) takes
/// the Aura. Khârn follows the Aura to P0, so P1 lost control and draws two.
/// The result survives a save/load and a full re-evaluation. At cleanup both
/// return to P1, and P0 draws two.
#[test]
fn word_of_seizing_on_control_magic_moves_the_enchanted_creature() {
    let WordBoard {
        mut runner,
        kharn,
        magic,
        word_events,
        word_previews,
        p1_library_before_word,
        ..
    } = word_board();
    assert!(
        word_previews > 0,
        "REACH: the layer 2 selector previewed Word of Seizing against Control Magic"
    );
    assert_eq!(
        controller(&runner, kharn),
        P0,
        "CR 109.5: Control Magic's current controller controls Khârn"
    );
    assert_eq!(changes_of(&word_events, kharn), vec![(P1, P0)]);
    assert_eq!(
        p1_library_before_word - library_size(&runner, P1),
        2,
        "P1 lost control of Khârn: draws two"
    );

    round_trip(&mut runner);
    evaluate_layers(runner.state_mut());
    assert_eq!(
        (controller(&runner, magic), controller(&runner, kharn)),
        (P0, P0),
        "a save/load and a full re-evaluation keep both with P0"
    );

    let p0_library = library_size(&runner, P0);
    let mut events = Vec::new();
    drive_to_end_of_turn(&mut runner, &mut events);
    assert_eq!(
        runner.state().phase,
        Phase::Cleanup,
        "REACH: Khârn's trigger gives priority in cleanup"
    );
    assert_eq!(
        controller(&runner, magic),
        P1,
        "REACH: Word of Seizing ended"
    );
    assert_eq!(
        controller(&runner, kharn),
        P1,
        "Khârn follows the Aura back"
    );
    assert_eq!(changes_of(&events, kharn), vec![(P0, P1)]);
    settle(&mut runner, &mut events);
    assert_eq!(
        p0_library - library_size(&runner, P0),
        2,
        "P0 lost control of Khârn: draws two"
    );
}

/// R2: Confiscate (a static control effect with a later timestamp) takes
/// Control Magic. Khârn follows Control Magic's new controller, P0.
#[test]
fn confiscate_on_control_magic_moves_the_enchanted_creature() {
    let mut scenario = board(2);
    let kharn = add_kharn(&mut scenario, P0);
    let magic = add_aura(&mut scenario, P1, "Control Magic", CONTROL_MAGIC);
    let confiscate = add_aura(&mut scenario, P0, "Confiscate", CONFISCATE);
    let mut runner = scenario.build();
    make_aura(&mut runner, magic, TypedFilter::creature());
    make_aura(&mut runner, confiscate, TypedFilter::permanent());
    cast_aura(&mut runner, P1, magic, kharn);
    assert_eq!(
        controller(&runner, kharn),
        P1,
        "REACH: Control Magic took Khârn"
    );

    let p1_library = library_size(&runner, P1);
    let events = cast_aura(&mut runner, P0, confiscate, magic);
    assert_eq!(
        controller(&runner, magic),
        P0,
        "REACH: Confiscate took Control Magic"
    );
    assert_eq!(
        controller(&runner, kharn),
        P0,
        "CR 109.5: Control Magic's current controller controls Khârn"
    );
    assert_eq!(changes_of(&events, kharn), vec![(P1, P0)]);
    assert_eq!(
        p1_library - library_size(&runner, P1),
        2,
        "P1 lost control of Khârn: draws two"
    );
}

/// R3: a three-link chain. P1's Control Magic takes P0's Khârn, P0's
/// Confiscate takes the Control Magic, and P2's Word of Seizing takes the
/// Confiscate. CR 613.8b: each effect waits for the one that changes its
/// source's controller, so all three end with P2.
#[test]
fn word_of_seizing_on_confiscate_on_control_magic_moves_the_whole_chain() {
    let mut scenario = board(3);
    let kharn = add_kharn(&mut scenario, P0);
    let magic = add_aura(&mut scenario, P1, "Control Magic", CONTROL_MAGIC);
    let confiscate = add_aura(&mut scenario, P0, "Confiscate", CONFISCATE);
    let word = add_word_of_seizing(&mut scenario, P2);
    let mut runner = scenario.build();
    make_aura(&mut runner, magic, TypedFilter::creature());
    make_aura(&mut runner, confiscate, TypedFilter::permanent());
    cast_aura(&mut runner, P1, magic, kharn);
    cast_aura(&mut runner, P0, confiscate, magic);
    assert_eq!(
        (controller(&runner, magic), controller(&runner, kharn)),
        (P0, P0),
        "REACH: Confiscate took Control Magic, and Khârn with it"
    );

    give_priority(&mut runner, P2);
    let p0_library = library_size(&runner, P0);
    let outcome = runner.cast(word).target_object(confiscate).resolve();
    let mut events = outcome.events().to_vec();
    assert_eq!(
        controller(&runner, confiscate),
        P2,
        "REACH: P2 controls Confiscate"
    );
    assert_eq!(
        controller(&runner, magic),
        P2,
        "CR 109.5: Confiscate's current controller controls Control Magic"
    );
    assert_eq!(
        controller(&runner, kharn),
        P2,
        "CR 109.5: Control Magic's current controller controls Khârn"
    );
    assert_eq!(changes_of(&events, kharn), vec![(P0, P2)]);
    settle(&mut runner, &mut events);
    assert_eq!(
        p0_library - library_size(&runner, P0),
        2,
        "P0 lost control of Khârn: draws two"
    );
}

/// R5 (regression guard): Control Magic and Act of Treason on the SAME
/// creature do not depend on each other, so they apply in timestamp order:
/// Act of Treason wins until cleanup, then Control Magic again. No pair needs
/// a scratch preview.
#[test]
fn control_magic_and_act_of_treason_on_one_creature_apply_in_timestamp_order() {
    let mut scenario = board(2);
    let creature = scenario.add_vanilla(P0, 2, 2);
    let magic = add_aura(&mut scenario, P1, "Control Magic", CONTROL_MAGIC);
    let treason = add_free_spell(
        &mut scenario,
        P0,
        "Act of Treason",
        CoreType::Sorcery,
        ACT_OF_TREASON,
    );
    let mut runner = scenario.build();
    make_aura(&mut runner, magic, TypedFilter::creature());
    cast_aura(&mut runner, P1, magic, creature);
    assert_eq!(
        controller(&runner, creature),
        P1,
        "REACH: Control Magic stole it"
    );

    perf_counters::reset();
    give_priority(&mut runner, P0);
    let outcome = runner.cast(treason).target_object(creature).resolve();
    let mut events = outcome.events().to_vec();
    assert_eq!(controller(&runner, creature), P0, "Act of Treason is newer");
    assert_eq!(controller(&runner, magic), P1);
    drive_to_end_of_turn(&mut runner, &mut events);
    assert_eq!(
        controller(&runner, creature),
        P1,
        "Control Magic again after cleanup"
    );
    assert_eq!(changes_of(&events, creature), vec![(P1, P0), (P0, P1)]);
    assert_eq!(
        perf_counters::control_dependency_snapshot().previews,
        0,
        "neither effect can change the other's source: no preview"
    );
}

/// R6 (hostile): after Word of Seizing gives P0 Control Magic and Khârn, P1
/// destroys the Control Magic. Khârn stays with its owner P0: no control
/// change, and nobody draws.
#[test]
fn destroying_a_stolen_control_magic_leaves_the_creature_with_its_owner() {
    let WordBoard {
        mut runner,
        kharn,
        magic,
        naturalize,
        ..
    } = word_board();
    assert_eq!(
        controller(&runner, kharn),
        P0,
        "REACH: Khârn followed the Aura"
    );

    let (p0_library, p1_library) = (library_size(&runner, P0), library_size(&runner, P1));
    give_priority(&mut runner, P1);
    let outcome = runner.cast(naturalize).target_object(magic).resolve();
    let mut events = outcome.events().to_vec();
    settle(&mut runner, &mut events);
    assert!(
        !runner.state().battlefield.contains(&magic),
        "REACH: Control Magic was destroyed"
    );
    assert_eq!(controller(&runner, kharn), P0);
    assert_eq!(changes_of(&events, kharn), vec![]);
    assert_eq!(
        (library_size(&runner, P0), library_size(&runner, P1)),
        (p0_library, p1_library)
    );
}

/// R7: a creature entering after the R1 steal takes the incremental layer
/// pass, whose layer 2 selector previews Word of Seizing against Control Magic
/// under the restricted recipient set. Its result matches a forced full pass
/// from the same state: the same controllers, transient effects, control
/// events and library sizes.
#[test]
fn incremental_layer_pass_after_the_steal_matches_a_full_pass() {
    let WordBoard {
        mut runner,
        kharn,
        magic,
        bear,
        ..
    } = word_board();
    assert_eq!(
        (controller(&runner, magic), controller(&runner, kharn)),
        (P0, P0),
        "REACH: both with P0"
    );
    give_priority(&mut runner, P0);
    let mut events = Vec::new();
    {
        let commit = runner.cast(bear).commit();
        events.extend_from_slice(commit.events());
    }
    act(&mut runner, GameAction::PassPriority, &mut events);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P1),
        "REACH: P1 holds priority over the creature spell"
    );
    let mut full = GameRunner::from_state(runner.state().clone());

    // A: the production flush.
    perf_counters::reset();
    let mut incremental_events = Vec::new();
    act(
        &mut runner,
        GameAction::PassPriority,
        &mut incremental_events,
    );
    let incremental_counters = perf_counters::snapshot();
    let incremental_previews = perf_counters::control_dependency_snapshot().previews;
    settle(&mut runner, &mut incremental_events);
    assert!(
        runner.state().battlefield.contains(&bear),
        "REACH: the creature entered"
    );
    assert!(
        incremental_counters.layers_incremental > 0,
        "REACH: the incremental arm ran: {incremental_counters:?}"
    );
    assert_eq!(incremental_counters.layers_escalated, 0);
    assert!(
        incremental_previews > 0,
        "REACH: the restricted layer 2 selector previewed the pair"
    );

    // B: the same state, forced to a full pass.
    full.state_mut().layers_dirty.mark_full();
    perf_counters::reset();
    let mut full_events = Vec::new();
    act(&mut full, GameAction::PassPriority, &mut full_events);
    let full_counters = perf_counters::snapshot();
    settle(&mut full, &mut full_events);
    assert!(
        full_counters.layers_full_eval > 0 && full_counters.layers_incremental == 0,
        "REACH: the forced-full arm ran: {full_counters:?}"
    );

    let controllers = |runner: &GameRunner| -> Vec<(ObjectId, PlayerId)> {
        let state = runner.state();
        state
            .battlefield
            .iter()
            .map(|id| (*id, state.objects[id].controller))
            .collect()
    };
    let transients = |runner: &GameRunner| -> Vec<_> {
        runner
            .state()
            .transient_continuous_effects
            .iter()
            .cloned()
            .collect()
    };
    let control_events = |events: &[GameEvent]| -> Vec<GameEvent> {
        events
            .iter()
            .filter(|event| matches!(event, GameEvent::ControllerChanged { .. }))
            .cloned()
            .collect()
    };
    assert_eq!(controllers(&runner), controllers(&full));
    assert!(controllers(&runner).contains(&(bear, P0)));
    assert!(controllers(&runner).contains(&(kharn, P0)));
    assert_eq!(transients(&runner), transients(&full));
    assert_eq!(control_events(&incremental_events), vec![]);
    assert_eq!(control_events(&full_events), vec![]);
    assert_eq!(
        (library_size(&runner, P0), library_size(&runner, P1)),
        (library_size(&full, P0), library_size(&full, P1))
    );
}

/// R8 (cost): a chain of Confiscates, each enchanting the one before, the first
/// enchanting P1's Island, all cast by P0. Only the Confiscate enchanting a
/// source can change that source's controller, so a full pass previews at most
/// one provider per remaining effect: k(k-1)/2 for k Confiscates, not the
/// cubic total of previewing every attached Aura.
#[test]
fn confiscate_chain_previews_only_the_enchanting_provider() {
    let mut scenario = board(2);
    let island = scenario.add_basic_land(P1, ManaColor::Blue);
    let confiscates: Vec<ObjectId> = (0..5)
        .map(|_| add_aura(&mut scenario, P0, "Confiscate", CONFISCATE))
        .collect();
    let mut runner = scenario.build();
    let mut host = island;
    for (k, &confiscate) in confiscates.iter().enumerate() {
        make_aura(&mut runner, confiscate, TypedFilter::permanent());
        cast_aura(&mut runner, P0, confiscate, host);
        assert_eq!(
            runner.state().objects[&confiscate]
                .attached_to
                .and_then(|target| target.as_object()),
            Some(host),
            "REACH: Confiscate {k} enchants the previous link"
        );
        perf_counters::reset();
        evaluate_layers(runner.state_mut());
        let links = k as u64 + 1;
        assert_eq!(
            perf_counters::control_dependency_snapshot().previews,
            links * (links - 1) / 2,
            "previews per full pass with {links} Confiscates"
        );
        host = confiscate;
    }
    assert_eq!(
        controller(&runner, island),
        P0,
        "REACH: the Island was taken"
    );
    for &confiscate in &confiscates {
        assert_eq!(controller(&runner, confiscate), P0);
    }
}

/// R9: the worked example. P1's Confiscate enchants P1's own Control Magic,
/// which enchants P0's creature; P0's Word of Seizing takes the Confiscate.
/// CR 613.8a + CR 613.8c: before Word applies, Confiscate would give Control
/// Magic to P1, who already controls it, so Control Magic is independent and
/// applies first; Confiscate waits for Word. P0 ends with both Auras and P1
/// keeps the creature.
#[test]
fn word_of_seizing_on_a_redundant_confiscate_leaves_the_creature_with_p1() {
    let mut scenario = board(2);
    let creature = scenario.add_vanilla(P0, 2, 2);
    let magic = add_aura(&mut scenario, P1, "Control Magic", CONTROL_MAGIC);
    let confiscate = add_aura(&mut scenario, P1, "Confiscate", CONFISCATE);
    let word = add_word_of_seizing(&mut scenario, P0);
    let mut runner = scenario.build();
    make_aura(&mut runner, magic, TypedFilter::creature());
    make_aura(&mut runner, confiscate, TypedFilter::permanent());
    cast_aura(&mut runner, P1, magic, creature);
    cast_aura(&mut runner, P1, confiscate, magic);
    assert_eq!(
        (controller(&runner, magic), controller(&runner, creature)),
        (P1, P1),
        "REACH: P1 controls Control Magic and the creature"
    );

    give_priority(&mut runner, P0);
    let outcome = runner.cast(word).target_object(confiscate).resolve();
    let mut events = outcome.events().to_vec();
    settle(&mut runner, &mut events);
    assert_eq!(
        controller(&runner, confiscate),
        P0,
        "REACH: P0 controls Confiscate"
    );
    assert_eq!(controller(&runner, magic), P0);
    assert_eq!(controller(&runner, creature), P1);
    assert_eq!(changes_of(&events, creature), vec![]);
}
