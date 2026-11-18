//! CR 608.2c + CR 201.5: an instruction that names the source (`~`, "this
//! creature") makes it the nearest antecedent of the next instruction's bare
//! "it". On a spell-cast trigger the condition also introduces the cast spell
//! (CR 109.2b); that spell must not capture the pronoun, or the card's untap or
//! return misses the card itself.

use engine::game::merge::{merge_object_onto, MergeSide};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::zone_pipeline::{move_object_for_test, ZoneMoveRequest};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

const BLISTERCOIL_WEIRD: &str = "Whenever you cast an instant or sorcery spell, this creature gets +1/+1 until end of turn. Untap it.";
const GWENNA: &str = "{T}: Add two mana in any combination of colors. Spend this mana only to cast creature spells or activate abilities of creature sources.\nWhenever you cast a creature spell with power 5 or greater, put a +1/+1 counter on Gwenna and untap it.";
const HIKARI: &str = "Flying\nWhenever you cast a Spirit or Arcane spell, you may exile Hikari. If you do, return it to the battlefield under its owner's control at the beginning of the next end step.";
const DIVINATION: &str = "Draw two cards.";
// Norin the Wary's printed text, with `~` standing for the host's name.
const NORIN_SHAPE: &str = "When a player casts a spell or a creature attacks, exile Wary Spirit. Return it to the battlefield under its owner's control at the beginning of the next end step.";
const REACH_THROUGH_MISTS: &str = "Draw a card.";

fn free() -> ManaCost {
    ManaCost::Cost {
        shards: vec![],
        generic: 0,
    }
}

fn scenario() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest"; 10]);
    scenario.with_library_top(P1, &["Forest"; 10]);
    scenario
}

/// Answer the one prompt in front of the game the way a player with no
/// further choices would: no attackers, no blockers, pass priority.
fn step(runner: &mut GameRunner) {
    let action = match runner.state().waiting_for.clone() {
        WaitingFor::OrderTriggers { .. } => {
            engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            return;
        }
        WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
            attacks: vec![],
            bands: vec![],
        },
        WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
            assignments: vec![],
        },
        WaitingFor::Priority { .. } => GameAction::PassPriority,
        other => panic!("unexpected prompt {other:?}"),
    };
    runner.act(action).expect("the no-choice answer is legal");
}

fn pass_until(runner: &mut GameRunner, done: impl Fn(&GameRunner) -> bool) {
    for _ in 0..600 {
        if done(runner) {
            return;
        }
        step(runner);
    }
    panic!(
        "never reached the awaited state; turn {} phase {:?} waiting {:?}",
        runner.state().turn_number,
        runner.state().phase,
        runner.state().waiting_for
    );
}

fn drain_stack(runner: &mut GameRunner) {
    pass_until(runner, |r| {
        r.state().stack.is_empty()
            && !matches!(r.state().waiting_for, WaitingFor::OrderTriggers { .. })
    });
}

fn zone(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

/// Move `id` to its owner's graveyard through the zone pipeline, so
/// replacement effects apply.
fn put_into_graveyard(runner: &mut GameRunner, id: ObjectId) {
    let mut events = Vec::new();
    move_object_for_test(
        runner.state_mut(),
        ZoneMoveRequest::effect(id, Zone::Graveyard, id),
        &mut events,
    );
    assert_eq!(zone(runner, id), Zone::Graveyard);
}

/// Drive to the next end step, where the delayed return must be the one
/// trigger on the stack, and resolve it.
fn resolve_the_end_step_return(runner: &mut GameRunner) {
    assert_eq!(runner.state().delayed_triggers.len(), 1);
    pass_until(runner, |r| r.state().phase == Phase::End);
    let stack: Vec<_> = runner.state().stack.iter().collect();
    assert!(
        matches!(
            stack.as_slice(),
            [entry] if matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })
        ),
        "the delayed return is on the stack at the end step: {stack:?}"
    );
    // CR 603.7b: a delayed triggered ability triggers only once.
    assert!(runner.state().delayed_triggers.is_empty());
    drain_stack(runner);
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

/// "this creature gets +1/+1 until end of turn. Untap it." untaps the Weird,
/// not the instant or sorcery spell that was cast.
#[test]
fn blistercoil_weird_untaps_itself_not_the_cast_spell() {
    let mut scenario = scenario();
    let weird = scenario
        .add_creature(P0, "Blistercoil Weird", 1, 1)
        .with_subtypes(vec!["Weird"])
        .from_oracle_text(BLISTERCOIL_WEIRD)
        .id();
    let divination = scenario
        .add_spell_to_hand_from_oracle(P0, "Divination", false, DIVINATION)
        .with_mana_cost(free())
        .id();
    let mut runner = scenario.build();
    runner.state_mut().objects.get_mut(&weird).unwrap().tapped = true;

    runner.cast(divination).resolve();
    drain_stack(&mut runner);

    // Reach guard: the trigger resolved its first instruction.
    assert_eq!(runner.state().objects[&weird].power, Some(2));
    assert!(
        !runner.state().objects[&weird].tapped,
        "\"Untap it.\" names the Weird the previous instruction named"
    );
    assert_eq!(zone(&runner, divination), Zone::Graveyard);
}

/// "put a +1/+1 counter on Gwenna and untap it" untaps Gwenna, not the
/// creature spell that was cast.
#[test]
fn gwenna_untaps_itself_after_counter() {
    let mut scenario = scenario();
    let gwenna = scenario
        .add_creature(P0, "Gwenna, Eyes of Gaea", 2, 3)
        .as_legendary()
        .with_subtypes(vec!["Elf", "Druid", "Scout"])
        .from_oracle_text(GWENNA)
        .id();
    let wurm = scenario
        .add_creature_to_hand(P0, "Craw Wurm", 6, 4)
        .with_mana_cost(free())
        .id();
    let mut runner = scenario.build();
    runner.state_mut().objects.get_mut(&gwenna).unwrap().tapped = true;

    runner.cast(wurm).resolve();
    drain_stack(&mut runner);

    // Reach guard: the trigger resolved its first instruction.
    assert_eq!(
        runner.state().objects[&gwenna]
            .counters
            .get(&CounterType::Plus1Plus1),
        Some(&1)
    );
    assert!(
        !runner.state().objects[&gwenna].tapped,
        "\"untap it\" names Gwenna, the object the previous instruction named"
    );
    // The cast spell is the other candidate referent; it gets no counter.
    assert_eq!(zone(&runner, wurm), Zone::Battlefield);
    assert!(runner.state().objects[&wurm].counters.is_empty());
}

fn hikari_scenario() -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = scenario();
    let hikari = scenario
        .add_creature(P0, "Hikari, Twilight Guardian", 4, 4)
        .as_legendary()
        .with_subtypes(vec!["Spirit"])
        .from_oracle_text(HIKARI)
        .id();
    let reach_through_mists = scenario
        .add_spell_to_hand_from_oracle(P0, "Reach Through Mists", true, REACH_THROUGH_MISTS)
        .with_subtypes(vec!["Arcane"])
        .with_mana_cost(free())
        .id();
    (scenario.build(), hikari, reach_through_mists)
}

/// CR 603.7e: "you may exile Hikari. If you do, return it …" returns Hikari
/// at the next end step.
#[test]
fn hikari_accept_returns_at_next_end_step() {
    let (mut runner, hikari, reach_through_mists) = hikari_scenario();

    runner.cast(reach_through_mists).accept_optional().resolve();
    drain_stack(&mut runner);
    assert_eq!(zone(&runner, hikari), Zone::Exile);
    assert_eq!(runner.state().delayed_triggers.len(), 1);

    resolve_the_end_step_return(&mut runner);
    assert_eq!(zone(&runner, hikari), Zone::Battlefield);
    assert_eq!(runner.state().objects[&hikari].controller, P0);
    assert_eq!(zone(&runner, reach_through_mists), Zone::Graveyard);
}

/// CR 730.3c + CR 603.7e: when the exiled source is a merged permanent, "return
/// it" finds every object that permanent became in exile, so the mutate rider
/// returns with Hikari, as a separate, unmerged object (CR 730.3).
#[test]
fn hikari_merged_returns_every_component() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = scenario();
    // Printed cards: the delayed return's CR 603.7a origin is keyed by them.
    let hikari = scenario.add_real_card(P0, "Hikari, Twilight Guardian", Zone::Battlefield, db);
    let rider = scenario.add_real_card(P0, "Gemrazer", Zone::Battlefield, db);
    let reach_through_mists = scenario
        .add_spell_to_hand_from_oracle(P0, "Reach Through Mists", true, REACH_THROUGH_MISTS)
        .with_subtypes(vec!["Arcane"])
        .with_mana_cost(free())
        .id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        let mut events = Vec::new();
        merge_object_onto(state, rider, hikari, MergeSide::Bottom, &mut events);
        state.battlefield.retain(|&id| id != rider);
    }

    runner.cast(reach_through_mists).accept_optional().resolve();
    drain_stack(&mut runner);
    assert_eq!(zone(&runner, hikari), Zone::Exile);
    assert_eq!(
        zone(&runner, rider),
        Zone::Exile,
        "reach guard: the pile split into exile"
    );

    resolve_the_end_step_return(&mut runner);
    for id in [hikari, rider] {
        let obj = &runner.state().objects[&id];
        assert_eq!(obj.zone, Zone::Battlefield, "{id:?} returns");
        assert_eq!(
            obj.controller, P0,
            "{id:?} returns under its owner's control"
        );
        assert!(
            obj.merged_components.is_empty() && obj.split_from_merge_survivor.is_none(),
            "{id:?} returns as a separate, unmerged object"
        );
    }
}

/// CR 730.3c + CR 603.7c: Norin's unconditional shape ("exile ~. Return it …")
/// returns through the tracked set its exile published; that return also finds
/// every object the merged permanent became. The host is a non-Human Spirit
/// carrying Norin's printed text, since mutate needs a non-Human host.
#[test]
fn norin_shape_merged_returns_every_component() {
    let mut scenario = scenario();
    let wary = scenario
        .add_creature(P0, "Wary Spirit", 2, 1)
        .with_subtypes(vec!["Spirit"])
        .from_oracle_text(NORIN_SHAPE)
        .id();
    let rider = scenario.add_creature(P0, "Gemrazer", 4, 4).id();
    let divination = scenario
        .add_spell_to_hand_from_oracle(P0, "Divination", false, DIVINATION)
        .with_mana_cost(free())
        .id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        let mut events = Vec::new();
        merge_object_onto(state, rider, wary, MergeSide::Bottom, &mut events);
        state.battlefield.retain(|&id| id != rider);
    }

    runner.cast(divination).resolve();
    drain_stack(&mut runner);
    assert_eq!(zone(&runner, wary), Zone::Exile);
    assert_eq!(
        zone(&runner, rider),
        Zone::Exile,
        "reach guard: the pile split into exile"
    );

    resolve_the_end_step_return(&mut runner);
    for id in [wary, rider] {
        let obj = &runner.state().objects[&id];
        assert_eq!(obj.zone, Zone::Battlefield, "{id:?} returns");
        assert!(
            obj.merged_components.is_empty() && obj.split_from_merge_survivor.is_none(),
            "{id:?} returns as a separate, unmerged object"
        );
    }
}

/// CR 608.2d: declining "you may exile Hikari" exiles nothing and schedules
/// no return.
#[test]
fn hikari_decline_stays_and_schedules_nothing() {
    let (mut runner, hikari, reach_through_mists) = hikari_scenario();

    let outcome = runner
        .cast(reach_through_mists)
        .commit()
        .stop_at_optional()
        .resolve();
    assert!(
        matches!(
            outcome.final_waiting_for(),
            WaitingFor::OptionalEffectChoice { player: P0, .. }
        ),
        "reach guard: Hikari's trigger offers the optional exile: {:?}",
        outcome.final_waiting_for()
    );
    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("declining the optional exile is legal");
    drain_stack(&mut runner);

    assert_eq!(zone(&runner, hikari), Zone::Battlefield);
    assert!(runner.state().delayed_triggers.is_empty());
    assert_eq!(zone(&runner, reach_through_mists), Zone::Graveyard);
}

/// CR 603.7c + CR 400.7: a Hikari that left exile before the end step is a
/// new object the delayed return does not find.
#[test]
fn hikari_moved_out_of_exile_is_not_returned() {
    let (mut runner, hikari, reach_through_mists) = hikari_scenario();

    runner.cast(reach_through_mists).accept_optional().resolve();
    drain_stack(&mut runner);
    assert_eq!(zone(&runner, hikari), Zone::Exile);
    put_into_graveyard(&mut runner, hikari);

    resolve_the_end_step_return(&mut runner);
    assert_eq!(zone(&runner, hikari), Zone::Graveyard);
}
