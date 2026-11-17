//! CR 613.1b + CR 603.2 + CR 603.10d: a control change a full layer evaluation
//! derives without an instruction (Control Magic, Confiscate, a lapsed "for as
//! long as" control duration) publishes `ControllerChanged`, so "when you lose
//! control" and "when you gain control" abilities trigger. Instruction-published
//! changes are not published a second time.
//!
//! This publishes every control change actually derived by a full layer
//! evaluation, without duplicating existing instruction publication. It does not
//! guarantee that every rules-required control transition is derived;
//! phase-rs/phase#10947 remains.
//!
//! Every row drives the production cast / activation / priority pipeline with
//! cards built from their verbatim Oracle text (Scryfall), or, for Mirror Room //
//! Fractured Realm, the card-data fixture.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{TargetFilter, TypedFilter};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::events::{ControlChangeEvidence, GameEvent};
use engine::types::game_state::{
    CastPaymentMode, GameRng, GameState, ManaChoice, PayCostKind, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use super::choose_creature_and_opponent_flip::seed_for_flip;

const P2: PlayerId = PlayerId(2);

const KHARN: &str = "Berzerker \u{2014} Kh\u{e2}rn the Betrayer attacks or blocks each combat if \
    able.\nSigil of Corruption \u{2014} When you lose control of Kh\u{e2}rn the Betrayer, draw two \
    cards.\nThe Betrayer \u{2014} If damage would be dealt to Kh\u{e2}rn the Betrayer, prevent that \
    damage and an opponent of your choice gains control of it.";
const ZIDANE: &str = "When Zidane enters, gain control of target creature an opponent \
    controls until end of turn. Untap it. It gains lifelink and haste until end of turn.\n\
    Whenever an opponent gains control of a permanent from you, you create a Treasure token.";
const CONTROL_MAGIC: &str = "Enchant creature\nYou control enchanted creature.";
const CONFISCATE: &str = "Enchant permanent\nYou control enchanted permanent.";
const NATURALIZE: &str = "Destroy target artifact or enchantment.";
const MURDER: &str = "Destroy target creature.";
const ACT_OF_TREASON: &str = "Gain control of target creature until end of turn. Untap that \
    creature. It gains haste until end of turn. (It can attack and {T} this turn.)";
const WORD_OF_SEIZING: &str = "Split second (As long as this spell is on the stack, players can't \
    cast spells or activate abilities that aren't mana abilities.)\nUntap target permanent and \
    gain control of it until end of turn. It gains haste until end of turn.";
const RUBINIA: &str = "You may choose not to untap Rubinia Soulsinger during your untap step.\n\
    {T}: Gain control of target creature for as long as you control Rubinia Soulsinger and \
    Rubinia Soulsinger remains tapped.";
const PHYREXIAN_ALTAR: &str = "Sacrifice a creature: Add one mana of any color.";
const ALADDIN: &str =
    "{1}{R}{R}, {T}: Gain control of target artifact for as long as you control this creature.";
const GUSTHAS_SCEPTER: &str = "{T}: Exile a card from your hand face down. You may look at it for \
    as long as it remains exiled.\n{T}: Return a card you own exiled with this artifact to your \
    hand.\nWhen you lose control of this artifact, put all cards exiled with this artifact into \
    their owner's graveyard.";
const GLORIOUS_ANTHEM: &str = "Creatures you control get +1/+1.";
const AUSTERE_COMMAND: &str = "Choose two \u{2014}\n\u{2022} Destroy all artifacts.\n\u{2022} \
    Destroy all enchantments.\n\u{2022} Destroy all creatures with mana value 3 or less.\n\
    \u{2022} Destroy all creatures with mana value 4 or greater.";
const ARTISANS_SORROW: &str = "Destroy target artifact or enchantment. Scry 2. (Look at the top \
    two cards of your library, then put any number of them on the bottom and the rest on top in \
    any order.)";
const RISKY_MOVE: &str = "At the beginning of each player's upkeep, that player gains control of \
    this enchantment.\nWhen you gain control of this enchantment from another player, choose a \
    creature you control and an opponent. Flip a coin. If you lose the flip, that opponent gains \
    control of that creature.";
const OPALESCENCE: &str = "Each other non-Aura enchantment is a creature in addition to its other \
    types and has base power and base toughness each equal to its mana value.";
const VEDALKEN_SHACKLES: &str = "You may choose not to untap this artifact during your untap \
    step.\n{2}, {T}: Gain control of target creature with power less than or equal to the number \
    of Islands you control for as long as this artifact remains tapped.";

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

fn treasures(runner: &GameRunner, player: PlayerId) -> usize {
    let state = runner.state();
    state
        .battlefield
        .iter()
        .filter(|id| {
            let obj = &state.objects[*id];
            obj.name == "Treasure" && obj.controller == player
        })
        .count()
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

fn evidence_of(events: &[GameEvent], object: ObjectId) -> Option<ControlChangeEvidence> {
    events.iter().find_map(|event| match event {
        GameEvent::ControllerChanged {
            object_id,
            evidence,
            ..
        } if *object_id == object => Some(*evidence),
        _ => None,
    })
}

/// Khârn as printed: {3}{R} (mana value 4), Legendary, 5/1.
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

fn add_zidane(scenario: &mut GameScenario, owner: PlayerId) -> ObjectId {
    scenario
        .add_creature_from_oracle(owner, "Zidane, Tantalus Thief", 3, 3, ZIDANE)
        .with_subtypes(vec!["Human", "Mutant", "Scout"])
        .as_legendary()
        .id()
}

fn add_rubinia(scenario: &mut GameScenario, owner: PlayerId) -> ObjectId {
    scenario
        .add_creature_from_oracle(owner, "Rubinia Soulsinger", 2, 3, RUBINIA)
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

/// A zero-cost instant or sorcery (`kind`) in `player`'s hand. The scenario
/// builder takes a bool; the conversion stays at that boundary.
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

fn add_word_of_seizing(scenario: &mut GameScenario, player: PlayerId) -> ObjectId {
    scenario
        .add_spell_to_hand(player, "Word of Seizing", true)
        .from_oracle_text_with_keywords(&["Split second"], WORD_OF_SEIZING)
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

/// Like [`settle`], answering Risky Move's choices: the chooser's only creature
/// (a single candidate is chosen for them) and their only opponent.
fn settle_answering_risky_move(runner: &mut GameRunner, events: &mut Vec<GameEvent>) {
    for _ in 0..40 {
        let action = match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::NamedChoice { player, .. } => GameAction::ChooseOption {
                choice: PlayerId(1 - player.0).0.to_string(),
            },
            other => panic!("unexpected prompt while resolving Risky Move: {other:?}"),
        };
        act(runner, action, events);
    }
    panic!("Risky Move's trigger never finished");
}

/// JSON save and load of the whole authoritative state.
fn round_trip(runner: &mut GameRunner) {
    runner.state_mut().capture_rng_word_pos();
    let saved = serde_json::to_string(runner.state()).expect("state serializes");
    let mut restored: GameState = serde_json::from_str(&saved).expect("state restores");
    restored.rehydrate_rng();
    *runner.state_mut() = restored;
}

fn kharn_board(players: u8) -> GameScenario {
    let mut scenario = if players == 2 {
        GameScenario::new()
    } else {
        GameScenario::new_n_player(players, 42)
    };
    scenario.at_phase(Phase::PreCombatMain);
    for player in 0..players {
        scenario.with_library_top(PlayerId(player), &LIBRARY);
    }
    scenario
}

/// T1: Control Magic takes Khârn, so P0 lost control and draws two. Destroying
/// Control Magic returns Khârn, so P1 lost control and draws two. Each change
/// publishes exactly one event, in the action that derived it.
#[test]
fn control_magic_on_and_off_fires_kharn_for_the_loser() {
    let mut scenario = kharn_board(2);
    let kharn = add_kharn(&mut scenario, P0);
    let magic = add_aura(&mut scenario, P1, "Control Magic", CONTROL_MAGIC);
    let naturalize = add_free_spell(
        &mut scenario,
        P1,
        "Naturalize",
        CoreType::Instant,
        NATURALIZE,
    );
    let mut runner = scenario.build();
    make_aura(&mut runner, magic, TypedFilter::creature());
    give_priority(&mut runner, P1);

    let (p0_lib, p1_lib) = (library_size(&runner, P0), library_size(&runner, P1));
    let outcome = runner.cast(magic).target_object(kharn).resolve();
    assert_eq!(changes_of(outcome.events(), kharn), vec![(P0, P1)]);
    runner.advance_until_stack_empty();
    assert_eq!(
        controller(&runner, kharn),
        P1,
        "REACH: Control Magic took Khârn"
    );
    assert_eq!(
        p0_lib - library_size(&runner, P0),
        2,
        "P0 lost control: draws two"
    );
    assert_eq!(library_size(&runner, P1), p1_lib);

    let p1_lib = library_size(&runner, P1);
    let p0_lib = library_size(&runner, P0);
    let outcome = runner.cast(naturalize).target_object(magic).resolve();
    assert_eq!(changes_of(outcome.events(), kharn), vec![(P1, P0)]);
    runner.advance_until_stack_empty();
    assert_eq!(
        controller(&runner, kharn),
        P0,
        "REACH: Khârn returned to P0"
    );
    assert_eq!(
        p1_lib - library_size(&runner, P1),
        2,
        "P1 lost control: draws two"
    );
    assert_eq!(library_size(&runner, P0), p0_lib);
}

/// T2 (phase-rs/phase#9903): Control Magic steals P0's creature while P0
/// controls Zidane: an opponent gained control of a permanent from P0, so P0
/// creates a Treasure.
#[test]
fn control_magic_steal_fires_zidane_for_the_victim() {
    let mut scenario = kharn_board(2);
    add_zidane(&mut scenario, P0);
    let victim = scenario.add_vanilla(P0, 2, 2);
    let magic = add_aura(&mut scenario, P1, "Control Magic", CONTROL_MAGIC);
    let mut runner = scenario.build();
    make_aura(&mut runner, magic, TypedFilter::creature());
    give_priority(&mut runner, P1);

    runner.cast(magic).target_object(victim).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(
        controller(&runner, victim),
        P1,
        "REACH: Control Magic stole it"
    );
    assert_eq!((treasures(&runner, P0), treasures(&runner, P1)), (1, 0));
}

/// T3: an instruction-published change is published once. Act of Treason's
/// steal is announced ahead of its realizing pass; its CR 514.2 cleanup
/// reversion is realized by the pass before it publishes. Each fires Khârn
/// once.
#[test]
fn act_of_treason_and_its_cleanup_reversion_publish_once() {
    let mut scenario = kharn_board(2);
    scenario.at_phase(Phase::PostCombatMain);
    let kharn = add_kharn(&mut scenario, P0);
    let treason = add_free_spell(
        &mut scenario,
        P1,
        "Act of Treason",
        CoreType::Sorcery,
        ACT_OF_TREASON,
    );
    let mut runner = scenario.build();
    give_priority(&mut runner, P1);

    let p0_lib = library_size(&runner, P0);
    let outcome = runner.cast(treason).target_object(kharn).resolve();
    assert_eq!(
        changes_of(outcome.events(), kharn),
        vec![(P0, P1)],
        "the steal publishes exactly one event"
    );
    runner.advance_until_stack_empty();
    assert_eq!(controller(&runner, kharn), P1, "REACH: P1 stole Khârn");
    assert_eq!(p0_lib - library_size(&runner, P0), 2);

    let turn = runner.state().turn_number;
    let p1_lib = library_size(&runner, P1);
    let mut events = Vec::new();
    drive_until(&mut runner, &mut events, "cleanup", |state| {
        state.phase == Phase::Cleanup || state.turn_number != turn
    });
    assert_eq!(runner.state().phase, Phase::Cleanup);
    assert_eq!(controller(&runner, kharn), P0, "REACH: control reverted");
    assert_eq!(
        changes_of(&events, kharn),
        vec![(P1, P0)],
        "the reversion publishes exactly one event"
    );
    assert_eq!(runner.state().stack.len(), 1, "exactly one trigger");
    settle(&mut runner, &mut events);
    assert_eq!(p1_lib - library_size(&runner, P1), 2);
}

/// T4 (a): Rubinia's "for as long as" steal is announced ahead of its
/// realizing pass and published once.
#[test]
fn rubinia_steal_publishes_once() {
    let mut scenario = kharn_board(2);
    let kharn = add_kharn(&mut scenario, P0);
    let rubinia = add_rubinia(&mut scenario, P1);
    let mut runner = scenario.build();
    give_priority(&mut runner, P1);

    let p0_lib = library_size(&runner, P0);
    let outcome = runner.activate(rubinia, 0).target_object(kharn).resolve();
    assert_eq!(changes_of(outcome.events(), kharn), vec![(P0, P1)]);
    runner.advance_until_stack_empty();
    assert_eq!(controller(&runner, kharn), P1, "REACH: Rubinia took Khârn");
    assert!(
        runner.state().objects[&rubinia].tapped,
        "REACH: Rubinia is tapped"
    );
    assert_eq!(p0_lib - library_size(&runner, P0), 2);
}

/// T4 (b): P1 chooses to untap Rubinia in P1's untap step (CR 502.3). Its
/// "for as long as" duration lapses, Khârn returns to P0, and P1's draw-two is
/// put on the stack before P1 receives priority in that upkeep.
#[test]
fn untapping_rubinia_lapses_its_steal_and_fires_kharn_for_the_thief() {
    let mut scenario = kharn_board(2);
    let kharn = add_kharn(&mut scenario, P0);
    let rubinia = add_rubinia(&mut scenario, P1);
    let mut runner = scenario.build();
    give_priority(&mut runner, P1);
    runner.activate(rubinia, 0).target_object(kharn).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(controller(&runner, kharn), P1, "REACH: Rubinia took Khârn");

    let mut events = Vec::new();
    drive_until(
        &mut runner,
        &mut events,
        "P1's untap choice",
        |state| matches!(state.waiting_for, WaitingFor::UntapChoice { player, .. } if player == P1),
    );
    let WaitingFor::UntapChoice { candidates, .. } = &runner.state().waiting_for else {
        unreachable!()
    };
    assert!(candidates.contains(&rubinia), "REACH: Rubinia is offered");
    let p1_lib = library_size(&runner, P1);
    let mut untap_events = Vec::new();
    act(
        &mut runner,
        GameAction::ChooseUntap {
            object_id: rubinia,
            untap: true,
        },
        &mut untap_events,
    );
    // A second candidate prompt (none here) would still be the untap step.
    assert!(
        !runner.state().objects[&rubinia].tapped,
        "REACH: Rubinia untapped"
    );
    assert_eq!(controller(&runner, kharn), P0, "REACH: the steal lapsed");
    assert_eq!(changes_of(&untap_events, kharn), vec![(P1, P0)]);
    assert_eq!(runner.state().phase, Phase::Upkeep);
    assert_eq!(runner.state().active_player, P1);
    assert_eq!(
        runner.state().stack.len(),
        1,
        "Khârn's trigger before priority"
    );
    assert_eq!(runner.state().stack[0].source_id, kharn);
    assert_eq!(runner.state().stack[0].controller, P1);
    settle(&mut runner, &mut untap_events);
    assert_eq!(p1_lib - library_size(&runner, P1), 2);
}

/// T5: a full re-evaluation that changes no controller publishes nothing.
/// Glorious Anthem re-derives Khârn (6/2) under Control Magic.
#[test]
fn a_full_pass_with_no_controller_change_publishes_nothing() {
    let mut scenario = kharn_board(2);
    let kharn = add_kharn(&mut scenario, P0);
    let magic = add_aura(&mut scenario, P1, "Control Magic", CONTROL_MAGIC);
    let anthem = scenario
        .add_spell_to_hand_from_oracle(P1, "Glorious Anthem", false, GLORIOUS_ANTHEM)
        .as_enchantment()
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    make_aura(&mut runner, magic, TypedFilter::creature());
    give_priority(&mut runner, P1);
    runner.cast(magic).target_object(kharn).resolve();
    runner.advance_until_stack_empty();

    let libs = (library_size(&runner, P0), library_size(&runner, P1));
    let outcome = runner.cast(anthem).resolve();
    runner.advance_until_stack_empty();
    let obj = &runner.state().objects[&kharn];
    assert_eq!(
        (obj.power, obj.toughness),
        (Some(6), Some(2)),
        "REACH: re-derived"
    );
    assert_eq!(obj.controller, P1);
    assert!(
        !outcome
            .events()
            .iter()
            .any(|event| matches!(event, GameEvent::ControllerChanged { .. })),
        "no control change, no event"
    );
    assert_eq!((library_size(&runner, P0), library_size(&runner, P1)), libs);
}

/// H2: P1 concedes (CR 800.4a). P1's Control Magic leaves with P1, Khârn returns
/// to P0, and the change is published. The trigger it fires is P1's, and P1 has
/// left the game, so nothing is drawn (CR 800.4d).
#[test]
fn conceding_thief_publishes_the_return_and_draws_nothing() {
    let mut scenario = kharn_board(3);
    let kharn = add_kharn(&mut scenario, P0);
    let magic = add_aura(&mut scenario, P1, "Control Magic", CONTROL_MAGIC);
    let mut runner = scenario.build();
    make_aura(&mut runner, magic, TypedFilter::creature());
    give_priority(&mut runner, P1);
    runner.cast(magic).target_object(kharn).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(
        controller(&runner, kharn),
        P1,
        "REACH: Control Magic took Khârn"
    );

    let (p0_lib, p2_lib) = (library_size(&runner, P0), library_size(&runner, P2));
    let mut events = Vec::new();
    act(
        &mut runner,
        GameAction::Concede { player_id: P1 },
        &mut events,
    );
    assert_eq!(
        controller(&runner, kharn),
        P0,
        "REACH: Khârn returned to P0"
    );
    assert_eq!(
        changes_of(&events, kharn),
        vec![(P1, P0)],
        "REACH: published"
    );
    assert!(
        runner.state().stack.is_empty(),
        "the departed player's trigger is gone"
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(
        (library_size(&runner, P0), library_size(&runner, P2)),
        (p0_lib, p2_lib)
    );
}

/// L1: P2's concession takes P2's Rubinia out of the game (CR 800.4a), so P1's
/// "for as long as you control Rubinia" steal ends and Khârn returns to P0. P1
/// lost control: P1's draw-two is on the stack before anyone receives priority.
#[test]
fn concession_that_ends_a_rubinia_steal_fires_kharn_for_the_thief() {
    let mut scenario = kharn_board(3);
    let kharn = add_kharn(&mut scenario, P0);
    let rubinia = add_rubinia(&mut scenario, P2);
    let word = add_word_of_seizing(&mut scenario, P1);
    let mut runner = scenario.build();
    give_priority(&mut runner, P1);

    runner.cast(word).target_object(rubinia).resolve();
    runner.advance_until_stack_empty();
    let p0_lib = library_size(&runner, P0);
    runner.activate(rubinia, 0).target_object(kharn).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(
        (controller(&runner, rubinia), controller(&runner, kharn)),
        (P1, P1),
        "REACH: P1 controls both"
    );
    assert!(
        runner.state().objects[&rubinia].tapped,
        "REACH: Rubinia is tapped"
    );
    assert_eq!(
        p0_lib - library_size(&runner, P0),
        2,
        "REACH: P0 lost Khârn"
    );

    let p1_lib = library_size(&runner, P1);
    let mut events = Vec::new();
    act(
        &mut runner,
        GameAction::Concede { player_id: P2 },
        &mut events,
    );
    assert_eq!(controller(&runner, kharn), P0, "REACH: the steal ended");
    assert_eq!(changes_of(&events, kharn), vec![(P1, P0)]);
    let evidence = evidence_of(&events, kharn).unwrap();
    assert!(matches!(evidence, ControlChangeEvidence::Observed(_)));
    let state = runner.state();
    assert_eq!(state.stack.len(), 1, "Khârn's trigger before priority");
    assert_eq!(state.stack[0].source_id, kharn);
    assert_eq!(state.stack[0].controller, P1);
    assert_eq!(state.waiting_for, WaitingFor::Priority { player: P1 });
    settle(&mut runner, &mut events);
    assert_eq!(p1_lib - library_size(&runner, P1), 2);
}

/// L2 (CR 514.2 + CR 514.3a + CR 603.10d): on P1's turn P1 seizes P0's Aladdin
/// and steals P0's Gustha's Scepter with it ("for as long as you control this
/// creature"), then exiles an Island with the Scepter. At cleanup Word of
/// Seizing ends and Aladdin returns to P0, so the Scepter's control duration
/// ends and its return to P0 is derived by the layer pass alone. P1 lost
/// control of the Scepter: its trigger is P1's, stacked in that same cleanup
/// step, and puts the Island into P1's graveyard.
#[test]
fn cleanup_aladdin_return_ends_its_steal_in_the_same_cleanup() {
    let mut scenario = kharn_board(2);
    scenario.at_phase(Phase::PostCombatMain);
    let aladdin = scenario
        .add_creature_from_oracle(P0, "Aladdin", 1, 1, ALADDIN)
        .with_subtypes(vec!["Human", "Rogue"])
        .id();
    let scepter = scenario
        .add_artifact_from_oracle(P0, "Gustha's Scepter", GUSTHAS_SCEPTER)
        .id();
    let word = add_word_of_seizing(&mut scenario, P1);
    let island = scenario.add_land_to_hand(P1, "Island").id();
    scenario.with_mana_pool(
        P1,
        (0..3)
            .map(|_| ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    give_priority(&mut runner, P1);

    runner.cast(word).target_object(aladdin).resolve();
    runner.advance_until_stack_empty();
    runner.activate(aladdin, 0).target_object(scepter).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(
        (controller(&runner, aladdin), controller(&runner, scepter)),
        (P1, P1),
        "REACH: P1 controls both"
    );

    give_priority(&mut runner, P1);
    let mut events = Vec::new();
    act(
        &mut runner,
        GameAction::ActivateAbility {
            source_id: scepter,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        },
        &mut events,
    );
    for _ in 0..8 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => act(&mut runner, GameAction::PassPriority, &mut events),
            WaitingFor::ChooseFromZoneChoice { cards, .. } => {
                assert!(cards.contains(&island), "REACH: the Island is offered");
                act(
                    &mut runner,
                    GameAction::SelectCards {
                        cards: vec![island],
                    },
                    &mut events,
                );
            }
            // "You may look at it for as long as it remains exiled."
            WaitingFor::OptionalEffectChoice { source_id, .. } if source_id == scepter => act(
                &mut runner,
                GameAction::DecideOptionalEffect { accept: true },
                &mut events,
            ),
            other => panic!("unexpected prompt while exiling with the Scepter: {other:?}"),
        }
    }
    assert_eq!(runner.state().objects[&island].zone, Zone::Exile, "REACH");
    assert!(
        runner
            .state()
            .exile_links
            .iter()
            .any(|link| link.source_id == scepter && link.exiled_id == island),
        "REACH: the Island is linked to the Scepter"
    );

    let turn = runner.state().turn_number;
    let mut events = Vec::new();
    drive_until(&mut runner, &mut events, "cleanup", |state| {
        state.phase == Phase::Cleanup || state.turn_number != turn
    });
    let state = runner.state();
    assert_eq!(state.turn_number, turn, "still P1's turn");
    assert_eq!(
        state.phase,
        Phase::Cleanup,
        "CR 514.3a: priority in cleanup"
    );
    assert_eq!(state.waiting_for, WaitingFor::Priority { player: P1 });
    assert_eq!(changes_of(&events, aladdin), vec![(P1, P0)]);
    assert_eq!(changes_of(&events, scepter), vec![(P1, P0)]);
    assert_eq!(state.stack.len(), 1, "exactly the Scepter's trigger");
    assert_eq!(state.stack[0].source_id, scepter);
    assert_eq!(state.stack[0].controller, P1);
    settle(&mut runner, &mut events);
    let island_obj = &runner.state().objects[&island];
    assert_eq!(island_obj.zone, Zone::Graveyard);
    assert_eq!(island_obj.owner, P1);
}

/// H3 (CR 603.10 + CR 603.3a): Confiscate gives P1 Risky Move. P1 gained control
/// of it from another player, so P1's Risky Move trigger chooses P1's creature
/// and opponent P0, and on a lost flip (CR 705.2) P0 gains that creature.
#[test]
fn confiscate_on_risky_move_fires_its_gain_trigger_to_completion() {
    let mut scenario = kharn_board(2);
    let risky = scenario
        .add_enchantment_from_oracle(P0, "Risky Move", RISKY_MOVE)
        .id();
    let bear = scenario.add_vanilla(P1, 2, 2);
    let other_bear = scenario.add_vanilla(P1, 1, 1);
    let confiscate = add_aura(&mut scenario, P1, "Confiscate", CONFISCATE);
    let mut runner = scenario.build();
    make_aura(&mut runner, confiscate, TypedFilter::permanent());
    give_priority(&mut runner, P1);

    let mut events = Vec::new();
    runner.cast(confiscate).target_object(risky).commit();
    drive_until(
        &mut runner,
        &mut events,
        "Risky Move's creature choice",
        |state| matches!(state.waiting_for, WaitingFor::ChooseFromZoneChoice { .. }),
    );
    assert_eq!(
        controller(&runner, risky),
        P1,
        "REACH: Confiscate took Risky Move"
    );
    assert_eq!(changes_of(&events, risky), vec![(P0, P1)]);
    let WaitingFor::ChooseFromZoneChoice {
        player,
        cards,
        source_id,
        ..
    } = &runner.state().waiting_for
    else {
        unreachable!()
    };
    assert_eq!((*player, *source_id), (P1, risky), "the gainer chooses");
    assert_eq!(
        cards
            .iter()
            .copied()
            .collect::<std::collections::HashSet<_>>(),
        std::collections::HashSet::from([bear, other_bear]),
        "P1's own creatures are offered"
    );
    act(
        &mut runner,
        GameAction::SelectCards { cards: vec![bear] },
        &mut events,
    );
    assert!(
        matches!(&runner.state().waiting_for, WaitingFor::NamedChoice { player, .. } if *player == P1),
        "the opponent choice: {:?}",
        runner.state().waiting_for
    );
    let seed = seed_for_flip(false);
    let state = runner.state_mut();
    state.rng_seed = seed;
    state.rng_word_pos = 0;
    state.rng = GameRng::seed_from_u64(seed);
    act(
        &mut runner,
        GameAction::ChooseOption {
            choice: P0.0.to_string(),
        },
        &mut events,
    );
    let flips: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            GameEvent::CoinFlipped { player_id, won } => Some((*player_id, *won)),
            _ => None,
        })
        .collect();
    assert_eq!(flips, vec![(P1, false)], "P1 flipped once and lost");
    assert_eq!(
        controller(&runner, bear),
        P0,
        "P0 gained the chosen creature"
    );
    assert_eq!(
        controller(&runner, other_bear),
        P1,
        "the other creature stays"
    );
    assert_eq!(changes_of(&events, bear), vec![(P1, P0)]);
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

/// Austere Command, control leg (CR 608.2c): "Destroy all enchantments" and
/// "Destroy all creatures with mana value 3 or less" destroy Control Magic and
/// leave Khârn (mana value 4), which returns to P0, so P1 draws two.
#[test]
fn austere_command_destroying_control_magic_returns_kharn_and_fires_for_the_thief() {
    let mut scenario = kharn_board(2);
    let kharn = add_kharn(&mut scenario, P0);
    let magic = add_aura(&mut scenario, P1, "Control Magic", CONTROL_MAGIC);
    let command = add_free_spell(
        &mut scenario,
        P1,
        "Austere Command",
        CoreType::Sorcery,
        AUSTERE_COMMAND,
    );
    let mut runner = scenario.build();
    make_aura(&mut runner, magic, TypedFilter::creature());
    give_priority(&mut runner, P1);
    runner.cast(magic).target_object(kharn).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(
        controller(&runner, kharn),
        P1,
        "REACH: Control Magic took Khârn"
    );

    let p1_lib = library_size(&runner, P1);
    let outcome = runner.cast(command).modes(&[1, 2]).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&magic].zone,
        Zone::Graveyard,
        "REACH"
    );
    assert_eq!(
        runner.state().objects[&kharn].zone,
        Zone::Battlefield,
        "REACH"
    );
    assert_eq!(controller(&runner, kharn), P0);
    assert_eq!(changes_of(outcome.events(), kharn), vec![(P1, P0)]);
    assert_eq!(p1_lib - library_size(&runner, P1), 2);
}

/// Austere Command, both creature modes' discriminator (CR 608.2c + CR 613.5):
/// "Destroy all enchantments" then "Destroy all creatures with mana value 4 or
/// greater". Control Magic is destroyed before Khârn, so Khârn returns to P0
/// while it is still on the battlefield and P1 draws two. The change is
/// derived for battlefield Khârn's exact incarnation (CR 400.7) before it
/// moves; it is published at the next drain, so in the action's event buffer
/// it follows Khârn's zone change.
#[test]
fn austere_command_returns_kharn_between_its_two_destroys() {
    let mut scenario = kharn_board(2);
    let kharn = add_kharn(&mut scenario, P0);
    let magic = add_aura(&mut scenario, P1, "Control Magic", CONTROL_MAGIC);
    let command = add_free_spell(
        &mut scenario,
        P1,
        "Austere Command",
        CoreType::Sorcery,
        AUSTERE_COMMAND,
    );
    let mut runner = scenario.build();
    make_aura(&mut runner, magic, TypedFilter::creature());
    give_priority(&mut runner, P1);
    runner.cast(magic).target_object(kharn).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(
        controller(&runner, kharn),
        P1,
        "REACH: Control Magic took Khârn"
    );
    let battlefield_incarnation = runner.state().objects[&kharn].incarnation;

    let (p0_lib, p1_lib) = (library_size(&runner, P0), library_size(&runner, P1));
    let mut events = Vec::new();
    runner.cast(command).modes(&[1, 3]).commit();
    drive_until(
        &mut runner,
        &mut events,
        "Austere Command resolved",
        |state| state.stack.iter().all(|entry| entry.source_id != command),
    );
    let state = runner.state();
    assert_eq!(state.objects[&magic].zone, Zone::Graveyard, "REACH");
    assert_eq!(state.objects[&kharn].zone, Zone::Graveyard, "REACH");
    assert_ne!(state.objects[&kharn].incarnation, battlefield_incarnation);
    assert_eq!(changes_of(&events, kharn), vec![(P1, P0)]);
    let evidence = evidence_of(&events, kharn).unwrap();
    let observation = state
        .control_change_observation(evidence)
        .expect("the trigger's event names its observation");
    assert_eq!(
        observation.subject.identity.incarnation,
        battlefield_incarnation
    );
    assert_eq!(observation.subject.zone, Zone::Battlefield);
    assert_eq!(observation.subject.controller, P1);
    settle(&mut runner, &mut events);
    assert_eq!(
        p1_lib - library_size(&runner, P1),
        2,
        "P1 lost control: draws two"
    );
    assert_eq!(library_size(&runner, P0), p0_lib);
}

/// Matt's witness board: P0 controls Opalescence, Risky Move (a 6/6 under it)
/// and a 2/2; P1's Vedalken Shackles and six Islands take Risky Move, and P1's
/// gain trigger resolves as a won flip. P0 has a free Austere Command.
struct ShacklesHoldRiskyMove<T> {
    runner: GameRunner,
    risky: ObjectId,
    bear: ObjectId,
    shackles: ObjectId,
    command: ObjectId,
    staged: T,
}

fn shackles_hold_risky_move<T>(
    stage: impl FnOnce(&mut GameScenario) -> T,
) -> ShacklesHoldRiskyMove<T> {
    let mut scenario = kharn_board(2);
    scenario
        .add_enchantment_from_oracle(P0, "Opalescence", OPALESCENCE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White, ManaCostShard::White],
            generic: 2,
        });
    let risky = scenario
        .add_enchantment_from_oracle(P0, "Risky Move", RISKY_MOVE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red; 3],
            generic: 3,
        })
        .id();
    let bear = scenario.add_vanilla(P0, 2, 2);
    let shackles = scenario
        .add_artifact_from_oracle(P1, "Vedalken Shackles", VEDALKEN_SHACKLES)
        .id();
    for _ in 0..6 {
        scenario.add_basic_land(P1, engine::types::mana::ManaColor::Blue);
    }
    scenario.with_mana_pool(
        P1,
        (0..2)
            .map(|_| ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]))
            .collect(),
    );
    let command = add_free_spell(
        &mut scenario,
        P0,
        "Austere Command",
        CoreType::Sorcery,
        AUSTERE_COMMAND,
    );
    let staged = stage(&mut scenario);
    let mut runner = scenario.build();
    // The scenario places objects directly; settle the board they form once.
    engine::game::layers::evaluate_layers(runner.state_mut());
    assert_eq!(
        runner.state().objects[&risky].power,
        Some(6),
        "REACH: Opalescence animates Risky Move as a 6/6"
    );

    give_priority(&mut runner, P1);
    let seed = seed_for_flip(true);
    let state = runner.state_mut();
    state.rng_seed = seed;
    state.rng_word_pos = 0;
    state.rng = GameRng::seed_from_u64(seed);
    let mut events = Vec::new();
    let outcome = runner.activate(shackles, 0).target_object(risky).resolve();
    events.extend(outcome.events().iter().cloned());
    settle_answering_risky_move(&mut runner, &mut events);
    assert_eq!(
        controller(&runner, risky),
        P1,
        "REACH: the Shackles took Risky Move"
    );
    assert!(runner.state().objects[&shackles].tapped, "REACH");
    assert!(
        events.iter().any(|event| matches!(
            event,
            GameEvent::CoinFlipped { player_id, won: true } if *player_id == P1
        )),
        "REACH: P1's gain trigger resolved as a win"
    );
    ShacklesHoldRiskyMove {
        runner,
        risky,
        bear,
        shackles,
        command,
        staged,
    }
}

/// P0 casts Austere Command choosing "Destroy all artifacts." then "Destroy all
/// enchantments." Destroying the Shackles ends its duration, so Risky Move
/// returns to P0 while it is still on the battlefield; the next mode destroys
/// it before the change is published. Returns the controllers of the Risky Move
/// triggers on the stack when P0 next receives priority.
fn austere_returns_then_destroys_risky_move<T>(
    board: &mut ShacklesHoldRiskyMove<T>,
) -> Vec<PlayerId> {
    let runner = &mut board.runner;
    give_priority(runner, P0);
    let mut events = Vec::new();
    runner.cast(board.command).modes(&[0, 1]).commit();
    let command = board.command;
    drive_until(runner, &mut events, "Austere Command resolved", |state| {
        state.stack.iter().all(|entry| entry.source_id != command)
    });
    let state = runner.state();
    assert_eq!(
        state.objects[&board.shackles].zone,
        Zone::Graveyard,
        "REACH"
    );
    assert_eq!(state.objects[&board.risky].zone, Zone::Graveyard, "REACH");
    assert_eq!(changes_of(&events, board.risky), vec![(P1, P0)]);
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    state
        .stack
        .iter()
        .filter(|entry| entry.source_id == board.risky)
        .map(|entry| entry.controller)
        .collect()
}

/// Matt's F1 witness (CR 603.10 + CR 113.7a), and the control for the
/// Fractured Realm row below: Risky Move existed immediately after it returned,
/// so P0's gain trigger triggered then, and it survives its source's
/// destruction. With no trigger doubler, exactly one P0 trigger, which resolves.
#[test]
fn shackles_destroyed_before_risky_move_still_fires_its_gain_trigger_for_p0() {
    let mut board = shackles_hold_risky_move(|_| ());
    let gain_triggers = austere_returns_then_destroys_risky_move(&mut board);
    assert_eq!(gain_triggers, vec![P0], "exactly one P0 gain trigger");

    let runner = &mut board.runner;
    let seed = seed_for_flip(false);
    let state = runner.state_mut();
    state.rng_seed = seed;
    state.rng_word_pos = 0;
    state.rng = GameRng::seed_from_u64(seed);
    let mut events = Vec::new();
    settle_answering_risky_move(runner, &mut events);
    let flips: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            GameEvent::CoinFlipped { player_id, won } => Some((*player_id, *won)),
            _ => None,
        })
        .collect();
    assert_eq!(flips, vec![(P0, false)], "P0 flipped once and lost");
    assert_eq!(
        controller(runner, board.bear),
        P1,
        "P1 gained P0's creature"
    );
}

/// Matt's round-2 witness (CR 603.2d + CR 603.10): the same board, plus P0's
/// Mirror Room // Fractured Realm cast as Fractured Realm ("If a triggered
/// ability of a permanent you control triggers, that ability triggers an
/// additional time."). Fractured Realm still functions when Risky Move returns
/// to P0, so P0's gain trigger triggers twice, although the same mode destroys
/// both enchantments before the change is published.
#[test]
fn fractured_realm_destroyed_with_risky_move_still_doubles_its_gain_trigger() {
    let mut board = shackles_hold_risky_move(|scenario| {
        let db = crate::support::shared_card_db().expect("card db");
        scenario.add_real_card(P0, "Mirror Room", Zone::Hand, db)
    });
    let room = board.staged;
    let runner = &mut board.runner;
    give_priority(runner, P0);
    let pool = &mut runner.state_mut().players[0].mana_pool;
    for _ in 0..7 {
        pool.add(ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]));
    }
    runner.cast(room).modal_back_face(true).resolve();
    let unlocks = runner.state().objects[&room].room_unlocks;
    assert_eq!(
        unlocks.map(|u| (u.left_unlocked, u.right_unlocked)),
        Some((false, true)),
        "REACH: cast as Fractured Realm, its door unlocked (CR 709.5d)"
    );
    assert!(
        runner.state().objects[&room]
            .static_definitions
            .iter_all()
            .any(|s| matches!(
                s.mode,
                engine::types::statics::StaticMode::DoubleTriggers { .. }
            )),
        "REACH: Fractured Realm's doubler functions"
    );
    assert_eq!(controller(runner, board.risky), P1, "REACH");

    let gain_triggers = austere_returns_then_destroys_risky_move(&mut board);
    assert_eq!(
        board.runner.state().objects[&room].zone,
        Zone::Graveyard,
        "REACH: Fractured Realm left with Risky Move"
    );
    assert_eq!(
        gain_triggers,
        vec![P0, P0],
        "Risky Move's gain trigger triggers an additional time"
    );
    let mut events = Vec::new();
    settle_answering_risky_move(&mut board.runner, &mut events);
}

/// Negative control (CR 400.7): Khârn destroyed while Control Magic still
/// enchants it changes zones, not control. Its new graveyard object reverting
/// to its owner publishes nothing, and nobody draws.
#[test]
fn destroying_the_stolen_creature_alone_publishes_no_control_change() {
    let mut scenario = kharn_board(2);
    let kharn = add_kharn(&mut scenario, P0);
    let magic = add_aura(&mut scenario, P1, "Control Magic", CONTROL_MAGIC);
    let murder = add_free_spell(&mut scenario, P0, "Murder", CoreType::Instant, MURDER);
    let mut runner = scenario.build();
    make_aura(&mut runner, magic, TypedFilter::creature());
    give_priority(&mut runner, P1);
    runner.cast(magic).target_object(kharn).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(
        controller(&runner, kharn),
        P1,
        "REACH: Control Magic took Khârn"
    );

    give_priority(&mut runner, P0);
    let (p0_lib, p1_lib) = (library_size(&runner, P0), library_size(&runner, P1));
    let mut events = Vec::new();
    runner.cast(murder).target_object(kharn).commit();
    settle(&mut runner, &mut events);
    assert_eq!(
        runner.state().objects[&kharn].zone,
        Zone::Graveyard,
        "REACH"
    );
    assert_eq!(
        runner.state().objects[&magic].zone,
        Zone::Graveyard,
        "REACH: the unattached Aura went to the graveyard (CR 704.5m)"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, GameEvent::ControllerChanged { .. })),
        "a zone change is not a control change"
    );
    assert_eq!(
        (library_size(&runner, P0), library_size(&runner, P1)),
        (p0_lib, p1_lib)
    );
}

/// S1a: a production pause, a JSON save and load, then resume, delivers exactly
/// once. Artisan's Sorrow destroys Control Magic and pauses on its scry.
#[test]
fn artisans_sorrow_scry_pause_round_trip_fires_kharn_exactly_once() {
    let mut scenario = kharn_board(2);
    let kharn = add_kharn(&mut scenario, P0);
    let magic = add_aura(&mut scenario, P1, "Control Magic", CONTROL_MAGIC);
    let sorrow = add_free_spell(
        &mut scenario,
        P0,
        "Artisan's Sorrow",
        CoreType::Instant,
        ARTISANS_SORROW,
    );
    let mut runner = scenario.build();
    make_aura(&mut runner, magic, TypedFilter::creature());
    give_priority(&mut runner, P1);
    runner.cast(magic).target_object(kharn).resolve();
    runner.advance_until_stack_empty();
    give_priority(&mut runner, P0);

    let p1_lib = library_size(&runner, P1);
    let mut events = Vec::new();
    runner.cast(sorrow).target_object(magic).commit();
    drive_until(&mut runner, &mut events, "the scry", |state| {
        matches!(state.waiting_for, WaitingFor::ScryChoice { .. })
    });
    assert_eq!(controller(&runner, kharn), P0, "REACH: Khârn returned");
    assert_eq!(changes_of(&events, kharn), vec![(P1, P0)]);
    let evidence = evidence_of(&events, kharn).unwrap();
    assert!(runner
        .state()
        .control_change_observation(evidence)
        .is_some());

    round_trip(&mut runner);
    let WaitingFor::ScryChoice { cards, .. } = runner.state().waiting_for.clone() else {
        panic!("the restored state is still at the scry");
    };
    act(&mut runner, GameAction::SelectCards { cards }, &mut events);
    settle(&mut runner, &mut events);
    assert_eq!(
        changes_of(&events, kharn),
        vec![(P1, P0)],
        "one event overall"
    );
    assert_eq!(p1_lib - library_size(&runner, P1), 2, "P1 draws two, once");
}

/// P1 (instruction plus choice): Act of Treason on Khârn with P0's Zidane in
/// play fires both of P0's look-back triggers, so the action ends at P0's
/// ordering choice. Both working-memory fields are empty at that boundary; a
/// save and load there, then the ordering answer, delivers each once.
#[test]
fn act_of_treason_order_triggers_round_trip_delivers_once() {
    let mut scenario = kharn_board(2);
    let kharn = add_kharn(&mut scenario, P0);
    add_zidane(&mut scenario, P0);
    let treason = add_free_spell(
        &mut scenario,
        P1,
        "Act of Treason",
        CoreType::Sorcery,
        ACT_OF_TREASON,
    );
    let mut runner = scenario.build();
    give_priority(&mut runner, P1);

    let p0_lib = library_size(&runner, P0);
    let mut events = Vec::new();
    runner.cast(treason).target_object(kharn).commit();
    drive_until(
        &mut runner,
        &mut events,
        "P0's ordering choice",
        |state| matches!(state.waiting_for, WaitingFor::OrderTriggers { player, .. } if player == P0),
    );
    assert!(runner.state().layer_control_changes.is_empty());
    assert!(runner.state().announced_control_changes.is_empty());
    round_trip(&mut runner);
    act(
        &mut runner,
        GameAction::OrderTriggers { order: vec![0, 1] },
        &mut events,
    );
    settle(&mut runner, &mut events);
    assert_eq!(changes_of(&events, kharn), vec![(P0, P1)]);
    assert_eq!(p0_lib - library_size(&runner, P0), 2);
    assert_eq!(treasures(&runner, P0), 1);
}

/// LC10-P: P1 sacrifices its tapped Rubinia to Phyrexian Altar, ending the
/// steal of P0's Khârn mid-mana-ability. The change is published at the
/// action's return while the colour prompt stays open; the game is saved and
/// loaded there; P1 draws two exactly once after the mana ability completes.
#[test]
fn altar_sacrifice_of_rubinia_publishes_kharns_return_across_a_save_at_the_colour_prompt() {
    let mut scenario = kharn_board(2);
    let kharn = add_kharn(&mut scenario, P0);
    let rubinia = add_rubinia(&mut scenario, P1);
    let altar = scenario
        .add_artifact_from_oracle(P1, "Phyrexian Altar", PHYREXIAN_ALTAR)
        .id();
    let mut runner = scenario.build();
    give_priority(&mut runner, P1);
    let p0_lib = library_size(&runner, P0);
    runner.activate(rubinia, 0).target_object(kharn).resolve();
    runner.advance_until_stack_empty();
    assert!(
        runner.state().objects[&rubinia].tapped,
        "REACH: Rubinia is tapped"
    );
    assert_eq!(controller(&runner, kharn), P1, "REACH: P1 controls Khârn");
    assert_eq!(
        p0_lib - library_size(&runner, P0),
        2,
        "REACH: P0's draw-two"
    );

    give_priority(&mut runner, P1);
    let (p0_lib, p1_lib) = (library_size(&runner, P0), library_size(&runner, P1));
    let mut events = Vec::new();
    act(
        &mut runner,
        GameAction::ActivateAbility {
            source_id: altar,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        },
        &mut events,
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::PayCost {
            kind: PayCostKind::Sacrifice,
            ..
        }
    ));
    let mut sacrifice = Vec::new();
    act(
        &mut runner,
        GameAction::SelectCards {
            cards: vec![rubinia],
        },
        &mut sacrifice,
    );
    let state = runner.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::ChooseManaColor { player, .. } if player == P1),
        "the colour prompt stays authoritative: {:?}",
        state.waiting_for
    );
    assert_eq!(state.objects[&rubinia].zone, Zone::Graveyard, "REACH");
    assert_eq!(controller(&runner, kharn), P0, "REACH: the steal ended");
    assert_eq!(changes_of(&sacrifice, kharn), vec![(P1, P0)]);
    let evidence = evidence_of(&sacrifice, kharn).unwrap();
    assert!(matches!(evidence, ControlChangeEvidence::Observed(_)));
    assert!(state.control_change_observation(evidence).is_some());
    assert_eq!(
        state.deferred_triggers.len(),
        1,
        "the carrier holds the trigger"
    );
    assert!(state.layer_control_changes.is_empty());
    assert!(state.announced_control_changes.is_empty());
    events.extend(sacrifice);

    round_trip(&mut runner);
    act(
        &mut runner,
        GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Black),
            count: 1,
        },
        &mut events,
    );
    settle(&mut runner, &mut events);
    assert_eq!(
        changes_of(&events, kharn),
        vec![(P1, P0)],
        "one event overall"
    );
    assert_eq!(p1_lib - library_size(&runner, P1), 2, "P1 draws two, once");
    assert_eq!(library_size(&runner, P0), p0_lib);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}
