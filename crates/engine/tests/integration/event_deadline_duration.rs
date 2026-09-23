//! U10-D: the event-deadline duration, "until a player casts <a|an> <filter> spell"
//! (CR 611.2a + CR 601.2i).
//!
//! Card rows use each card's verbatim Oracle text and drive the real cast and
//! activation pipeline. Soul Sculptor's rows assert its abilities reading only;
//! its card-type reading ("becomes an enchantment") belongs to a later phase.
//! The preservation rows guard the neighbouring "until …" readings the new
//! duration must leave untouched.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{AbilityKind, CastingPermission, Duration, Effect};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const SOUL_SCULPTOR: &str = "{1}{W}, {T}: Target creature becomes an enchantment and loses all abilities until a player casts a creature spell.";
const GOBLIN_CHARBELCHER: &str = "{3}, {T}: Reveal cards from the top of your library until you reveal a land card. This artifact deals damage equal to the number of nonland cards revealed this way to any target. If the revealed land card was a Mountain, this artifact deals double that damage instead. Put the revealed cards on the bottom of your library in any order.";
const FURIOUS_RISE: &str = "At the beginning of your end step, if you control a creature with power 4 or greater, exile the top card of your library. You may play that card until you exile another card with this enchantment.";
const PALACE_JAILER: &str = "When this creature enters, you become the monarch.\nWhen this creature enters, exile target creature an opponent controls until an opponent becomes the monarch.";
const TANGLEROOT: &str = "Whenever a player casts a creature spell, that player adds {G}.";
const UNYARO: &str = "At the beginning of your end step, if you planeswalked to Unyaro this turn, untap all creatures. They phase out until a player planeswalks. (Treat them and anything attached to them as though they didn't exist.)\nWhenever chaos ensues, create two 2/2 white and blue Knight creature tokens with vigilance.";

/// The target: a flier that also gains its controller life on every creature cast.
const WATCHER: &str = "Flying\nWhenever a player casts a creature spell, you gain 1 life.";

fn mana(kinds: &[ManaType]) -> Vec<ManaUnit> {
    kinds
        .iter()
        .map(|kind| ManaUnit::new(*kind, ObjectId(0), false, vec![]))
        .collect()
}

fn zero_cost_creature(scenario: &mut GameScenario, name: &str) -> ObjectId {
    scenario
        .add_creature_to_hand_from_oracle(P0, name, 2, 2, "")
        .with_mana_cost(ManaCost::generic(0))
        .id()
}

fn zero_cost_instant(scenario: &mut GameScenario, name: &str, text: &str) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, name, true, text)
        .with_mana_cost(ManaCost::generic(0))
        .id()
}

fn activated_index(runner: &GameRunner, source: ObjectId) -> usize {
    runner.state().objects[&source]
        .abilities
        .iter()
        .position(|ability| ability.kind == AbilityKind::Activated)
        .expect("the source has an activated ability")
}

fn life(runner: &GameRunner, player: PlayerId) -> i32 {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .life
}

fn on_stack(runner: &GameRunner, object: ObjectId) -> bool {
    runner.state().stack.iter().any(|entry| entry.id == object)
}

fn stack_entries_named(runner: &GameRunner, name: &str) -> usize {
    runner
        .state()
        .stack
        .iter()
        .filter(|entry| {
            runner
                .state()
                .objects
                .get(&entry.id)
                .is_some_and(|object| object.name == name)
        })
        .count()
}

/// Pass priority until the top of the stack changes (it resolved).
fn resolve_top_entry(runner: &mut GameRunner) {
    let top = runner.state().stack.last().map(|entry| entry.id);
    for _ in 0..10 {
        if runner.state().stack.last().map(|entry| entry.id) != top {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("passing priority is legal");
    }
    panic!("the top of the stack did not resolve");
}

// ---------------------------------------------------------------------------
// Soul Sculptor (M-11 (b))
// ---------------------------------------------------------------------------

struct SculptorBoard {
    runner: GameRunner,
    sculptor: ObjectId,
    watcher: ObjectId,
    bear: ObjectId,
    late_bear: ObjectId,
    instant: ObjectId,
    copier: ObjectId,
    destroy: ObjectId,
    reanimate: ObjectId,
    buried: ObjectId,
}

fn sculptor_board() -> SculptorBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, mana(&[ManaType::White, ManaType::Colorless]));
    let sculptor = scenario
        .add_creature_from_oracle(P0, "Soul Sculptor", 1, 1, SOUL_SCULPTOR)
        .id();
    let watcher = scenario
        .add_creature_from_oracle(P1, "Watcher", 1, 1, WATCHER)
        .id();
    let bear = zero_cost_creature(&mut scenario, "Cast Bear");
    let late_bear = zero_cost_creature(&mut scenario, "Late Bear");
    let instant = zero_cost_instant(&mut scenario, "Gain Instant", "You gain 1 life.");
    let copier = zero_cost_instant(&mut scenario, "Twin Instant", "Copy target creature spell.");
    let destroy = zero_cost_instant(&mut scenario, "Destroy Instant", "Destroy target creature.");
    let reanimate = zero_cost_instant(
        &mut scenario,
        "Reanimate Instant",
        "Return target creature card from your graveyard to the battlefield.",
    );
    let buried = scenario
        .add_creature_to_graveyard(P0, "Buried Bear", 2, 2)
        .id();
    SculptorBoard {
        runner: scenario.build(),
        sculptor,
        watcher,
        bear,
        late_bear,
        instant,
        copier,
        destroy,
        reanimate,
        buried,
    }
}

fn activate_sculptor(board: &mut SculptorBoard) {
    let index = activated_index(&board.runner, board.sculptor);
    board
        .runner
        .activate(board.sculptor, index)
        .target_object(board.watcher)
        .resolve();
}

/// Activate Soul Sculptor on the watcher, leaving the ability on the stack.
fn activate_sculptor_onto_stack(board: &mut SculptorBoard) {
    let index = activated_index(&board.runner, board.sculptor);
    board
        .runner
        .act(GameAction::ActivateAbility {
            source_id: board.sculptor,
            ability_index: index,
        })
        .expect("the activation is legal");
    while matches!(
        board.runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ) {
        board
            .runner
            .act(GameAction::ChooseTarget {
                target: Some(engine::types::ability::TargetRef::Object(board.watcher)),
            })
            .expect("the watcher is a legal target");
    }
}

fn watcher_has_abilities(runner: &GameRunner, watcher: ObjectId) -> bool {
    let object = &runner.state().objects[&watcher];
    object.has_keyword(&Keyword::Flying) && !object.trigger_definitions.is_empty()
}

fn watcher_has_no_abilities(runner: &GameRunner, watcher: ObjectId) -> bool {
    let object = &runner.state().objects[&watcher];
    !object.has_keyword(&Keyword::Flying) && object.trigger_definitions.is_empty()
}

/// SS-1: the target has no abilities through a copied creature spell and a
/// noncreature spell; once a creature spell becomes cast it has them again,
/// read before that spell resolves (CR 611.2a + CR 601.2i), and its regained
/// trigger triggers on that cast.
#[test]
fn soul_sculptor_ends_when_a_creature_spell_becomes_cast() {
    let mut board = sculptor_board();

    // The copy leg first: the creature spell is cast before the effect begins
    // and copied after it, so the copy is the only candidate event (CR 707.10).
    let _ = board.runner.cast(board.bear).commit();
    activate_sculptor_onto_stack(&mut board);
    resolve_top_entry(&mut board.runner);
    assert!(
        watcher_has_no_abilities(&board.runner, board.watcher),
        "reach: the watcher lost all abilities immediately before the copy"
    );
    let _ = board
        .runner
        .cast(board.copier)
        .target_object(board.bear)
        .commit();
    resolve_top_entry(&mut board.runner);
    assert_eq!(
        stack_entries_named(&board.runner, "Cast Bear"),
        2,
        "reach: the copier put a copy of the creature spell on the stack"
    );
    assert!(
        watcher_has_no_abilities(&board.runner, board.watcher),
        "CR 707.10: a copy of a creature spell is not cast, so the effect continues"
    );
    board.runner.advance_until_stack_empty();
    assert!(
        watcher_has_no_abilities(&board.runner, board.watcher),
        "CR 611.2a: the effect continues after the copy and the original resolve"
    );

    board.runner.cast(board.instant).resolve();
    assert!(
        watcher_has_no_abilities(&board.runner, board.watcher),
        "CR 611.2a: a noncreature spell does not end the effect"
    );

    let life_before = life(&board.runner, P1);
    let _ = board.runner.cast(board.late_bear).commit();
    assert!(
        on_stack(&board.runner, board.late_bear),
        "reach: the creature spell has not resolved"
    );
    assert!(
        watcher_has_abilities(&board.runner, board.watcher),
        "CR 611.2a + CR 601.2i: the watcher has its abilities again once a creature spell \
         becomes cast, before that spell resolves"
    );
    resolve_top_entry(&mut board.runner);
    assert!(
        on_stack(&board.runner, board.late_bear),
        "reach: the regained trigger resolved above the creature spell"
    );
    assert_eq!(
        life(&board.runner, P1) - life_before,
        1,
        "CR 601.2i + CR 603.10: the regained trigger triggers on the cast that ended the effect"
    );
}

/// SS-2: the effect does not end with its source leaving the battlefield
/// (CR 611.2a states the duration), nor with a creature put onto the
/// battlefield without being cast; a later creature cast ends it.
#[test]
fn soul_sculptor_effect_outlives_its_source_until_a_creature_cast() {
    let mut board = sculptor_board();
    activate_sculptor(&mut board);
    assert!(
        watcher_has_no_abilities(&board.runner, board.watcher),
        "reach: the watcher lost all abilities"
    );

    board
        .runner
        .cast(board.destroy)
        .target_object(board.sculptor)
        .resolve();
    assert_eq!(
        board.runner.state().objects[&board.sculptor].zone,
        Zone::Graveyard,
        "reach: Soul Sculptor left the battlefield"
    );
    board
        .runner
        .cast(board.reanimate)
        .target_object(board.buried)
        .resolve();
    assert_eq!(
        board.runner.state().objects[&board.buried].zone,
        Zone::Battlefield,
        "reach: a creature was put onto the battlefield without being cast"
    );
    assert!(
        watcher_has_no_abilities(&board.runner, board.watcher),
        "CR 611.2a: neither the source leaving nor an uncast creature ends the effect"
    );

    let _ = board.runner.cast(board.bear).commit();
    assert!(
        watcher_has_abilities(&board.runner, board.watcher),
        "CR 611.2a + CR 601.2i: a creature spell becoming cast ends the effect"
    );
}

/// SS-3: Soul Sculptor's ability from a token source; the token leaves the
/// battlefield and ceases to exist in the state-based-action pass
/// (CR 704.5d). A creature spell cast afterwards still ends the effect, read
/// before that spell resolves. The card-source board is the reach guard.
#[test]
fn soul_sculptor_effect_ends_after_its_token_source_ceased_to_exist() {
    for token in [false, true] {
        let mut board = sculptor_board();
        if token {
            board
                .runner
                .state_mut()
                .objects
                .get_mut(&board.sculptor)
                .expect("Soul Sculptor exists")
                .is_token = true;
        }
        activate_sculptor(&mut board);
        board
            .runner
            .cast(board.destroy)
            .target_object(board.sculptor)
            .resolve();
        assert_eq!(
            board.runner.state().objects.contains_key(&board.sculptor),
            !token,
            "reach: token={token}: a destroyed token ceases to exist (CR 704.5d), a card \
             stays in the graveyard"
        );
        assert!(
            watcher_has_no_abilities(&board.runner, board.watcher),
            "reach: token={token}: the effect outlived its source"
        );

        let _ = board.runner.cast(board.bear).commit();
        assert!(
            on_stack(&board.runner, board.bear),
            "reach: token={token}: the creature spell has not resolved"
        );
        assert!(
            watcher_has_abilities(&board.runner, board.watcher),
            "CR 611.2a + CR 601.2i: token={token}: the effect ends at the creature cast"
        );
    }
}

// ---------------------------------------------------------------------------
// Preservation rows (M-11 (c))
// ---------------------------------------------------------------------------

/// P-1: Unyaro's "until a player planeswalks" is not a spell-cast deadline,
/// so its phase-out does not take the event-deadline duration.
#[test]
fn unyaro_phase_out_is_not_a_spell_cast_deadline() {
    let parsed = parse_oracle_text(
        UNYARO,
        "Unyaro",
        &[],
        &["Plane".to_string()],
        &["Zhalfir".to_string()],
    );
    let phase_out = parsed
        .triggers
        .first()
        .and_then(|trigger| trigger.execute.as_deref())
        .and_then(|execute| execute.sub_ability.as_deref())
        .expect("reach: the end-step trigger untaps, then phases out");
    assert!(
        matches!(*phase_out.effect, Effect::PhaseOut { .. }),
        "reach: the sub-ability is the phase-out"
    );
    assert!(
        !matches!(phase_out.duration, Some(Duration::UntilEvent { .. })),
        "\"until a player planeswalks\" is not a spell-cast deadline"
    );
}

/// P-2: Goblin Charbelcher's "until you reveal a land card" is the reveal
/// loop's stop condition, not a duration: the loop stops at the first land, so
/// the card beneath it is never revealed and stays on top, and the nonland
/// cards revealed before it leave the top of the library. This row guards the
/// loop only.
#[test]
fn goblin_charbelcher_reveals_until_a_land_card() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        mana(&[
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
        ]),
    );
    let belcher = scenario
        .add_artifact_from_oracle(P0, "Goblin Charbelcher", GOBLIN_CHARBELCHER)
        .id();
    let deep = scenario.add_card_to_library_top(P0, "Deep Card");
    let land = scenario.add_card_to_library_top(P0, "Revealed Land");
    let second = scenario.add_card_to_library_top(P0, "Second Spell");
    let first = scenario.add_card_to_library_top(P0, "First Spell");
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&land)
        .expect("the land exists")
        .card_types
        .core_types = vec![CoreType::Land];
    let index = activated_index(&runner, belcher);
    assert!(
        matches!(
            *runner.state().objects[&belcher].abilities[index].effect,
            Effect::RevealUntil { .. }
        ),
        "the activated ability is the reveal-until loop"
    );

    runner.activate(belcher, index).target_player(P1).resolve();
    runner.advance_until_stack_empty();
    let library = &runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .expect("P0 exists")
        .library;
    assert_eq!(
        library.front(),
        Some(&deep),
        "the loop stopped at the land: the card beneath it was not revealed"
    );
    for revealed in [first, second] {
        assert!(
            library.iter().skip(1).any(|id| *id == revealed),
            "revealed nonland card {revealed:?} left the top of the library"
        );
    }
}

/// P-3: Furious Rise's permission survives an intervening cast and ends when
/// the same enchantment exiles another card (CR 607.2a + CR 611.2a).
#[test]
fn furious_rise_permission_ends_only_when_it_exiles_another_card() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let rise = scenario
        .add_creature(P0, "Furious Rise", 0, 0)
        .as_enchantment()
        .from_oracle_text(FURIOUS_RISE)
        .id();
    scenario.add_creature(P0, "Big Creature", 5, 5);
    scenario.with_library_top(
        P0,
        &["First Exiled", "Drawn Card", "Second Exiled", "Filler"],
    );
    scenario.with_library_top(P1, &["P1 Draw One", "P1 Draw Two", "P1 Draw Three"]);
    let instant = zero_cost_instant(&mut scenario, "Gain Instant", "You gain 1 life.");
    let mut runner = scenario.build();
    let first = card_named(&runner, "First Exiled");
    let second = card_named(&runner, "Second Exiled");

    reach_own_end_step(&mut runner);
    assert_eq!(
        runner.state().objects[&first].zone,
        Zone::Exile,
        "reach: Furious Rise exiled the top card"
    );
    assert!(
        has_play_permission_from(&runner, first, rise),
        "reach: the exiled card may be played"
    );

    runner.cast(instant).resolve();
    assert!(
        has_play_permission_from(&runner, first, rise),
        "CR 611.2a: an intervening cast does not end the permission"
    );

    reach_own_end_step(&mut runner);
    assert_eq!(
        runner.state().active_player,
        P0,
        "reach: the second end step is P0's"
    );
    assert_eq!(
        runner.state().objects[&second].zone,
        Zone::Exile,
        "reach: Furious Rise exiled another card"
    );
    assert!(
        !has_play_permission_from(&runner, first, rise),
        "CR 607.2a + CR 611.2a: the first card's permission ends when the enchantment exiles \
         another card"
    );
    assert!(
        has_play_permission_from(&runner, second, rise),
        "the newly exiled card may be played"
    );
}

fn card_named(runner: &GameRunner, name: &str) -> ObjectId {
    runner
        .state()
        .objects
        .values()
        .find(|object| object.name == name)
        .map(|object| object.id)
        .expect("the named card exists")
}

fn has_play_permission_from(runner: &GameRunner, card: ObjectId, source: ObjectId) -> bool {
    runner.state().objects[&card]
        .casting_permissions
        .iter()
        .any(|permission| {
            matches!(
                permission,
                CastingPermission::PlayFromExile { source_id: Some(id), .. } if *id == source
            )
        })
}

/// Pass priority through the turn structure until P0's next end step has put
/// Furious Rise's trigger on the stack, then resolve it.
fn reach_own_end_step(runner: &mut GameRunner) {
    for _ in 0..300 {
        let state = runner.state();
        if state.active_player == P0 && state.phase == Phase::End && !state.stack.is_empty() {
            runner.advance_until_stack_empty();
            return;
        }
        let action = if matches!(state.waiting_for, WaitingFor::DeclareAttackers { .. }) {
            GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            }
        } else {
            GameAction::PassPriority
        };
        runner
            .act(action)
            .expect("an empty attack declaration or a priority pass is legal");
    }
    panic!("P0's end-step trigger was never put on the stack");
}

/// P-4: Palace Jailer's exile lasts until an opponent becomes the monarch
/// (CR 610.3), and a spell cast in between does not end it.
#[test]
fn palace_jailer_returns_the_creature_when_an_opponent_becomes_monarch() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let jailer = scenario
        .add_creature_to_hand_from_oracle(P0, "Palace Jailer", 2, 2, PALACE_JAILER)
        .id();
    let exiled = scenario.add_creature(P1, "Exiled Creature", 2, 2).id();
    let bear = zero_cost_creature(&mut scenario, "Cast Bear");
    let crown = zero_cost_instant(
        &mut scenario,
        "Crown Opponent",
        "Target opponent becomes the monarch.",
    );
    let mut runner = scenario.build();

    runner.cast(jailer).target_object(exiled).resolve();
    assert_eq!(
        runner.state().objects[&exiled].zone,
        Zone::Exile,
        "reach: Palace Jailer exiled the creature"
    );
    assert_eq!(runner.state().monarch, Some(P0), "reach: P0 is the monarch");

    runner.cast(bear).resolve();
    assert_eq!(
        runner.state().objects[&exiled].zone,
        Zone::Exile,
        "CR 610.3: a creature cast does not end the exile"
    );

    runner.cast(crown).target_player(P1).resolve();
    assert_eq!(
        runner.state().monarch,
        Some(P1),
        "reach: P1 became the monarch"
    );
    assert_eq!(
        runner.state().objects[&exiled].zone,
        Zone::Battlefield,
        "CR 610.3: the creature returns when an opponent becomes the monarch"
    );
}

/// P-5: Tangleroot's spell-cast trigger fires once per creature spell cast,
/// and not for a noncreature spell.
#[test]
fn tangleroot_adds_green_once_per_creature_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_artifact_from_oracle(P1, "Tangleroot", TANGLEROOT);
    let bear = zero_cost_creature(&mut scenario, "Cast Bear");
    let late_bear = zero_cost_creature(&mut scenario, "Late Bear");
    let instant = zero_cost_instant(&mut scenario, "Gain Instant", "You gain 1 life.");
    let mut runner = scenario.build();
    let green = |runner: &GameRunner| {
        runner
            .state()
            .players
            .iter()
            .find(|p| p.id == P0)
            .expect("P0 exists")
            .mana_pool
            .count_color(ManaType::Green)
    };

    runner.cast(bear).resolve();
    assert_eq!(green(&runner), 1, "the first creature cast adds one {{G}}");
    runner.cast(instant).resolve();
    assert_eq!(green(&runner), 1, "a noncreature cast adds nothing");
    runner.cast(late_bear).resolve();
    assert_eq!(
        green(&runner),
        2,
        "the second creature cast adds one more {{G}}"
    );
}
