//! CR 120.4a + CR 120.3d: Ram Through's "excess damage is dealt to that
//! creature's controller instead" splits the damage before it is applied, so a
//! wither/infect source puts only the lethal portion on the creature as -1/-1
//! counters (CR 702.80a / CR 702.90c) and the excess reaches the controller
//! (poison for infect, CR 702.90b). Deathtouch makes lethal a flat 1.

use engine::game::scenario::{CastOutcome, GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{
    GameState, PersistedGameState, PersistedRestoreFinalization, ReplacementChoiceKind, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const RAM_THROUGH: &str = "Target creature you control deals damage equal to its power \
to target creature you don't control. If the creature you control has trample, excess \
damage is dealt to that creature's controller instead.";
const BLIGHTSTEEL: &str = "Trample, infect, indestructible\nIf Blightsteel Colossus would be \
put into a graveyard from anywhere, reveal Blightsteel Colossus and shuffle it into its \
owner's library instead.";
const PUTREFAX: &str = "Trample, haste\nInfect (This creature deals damage to creatures in the \
form of -1/-1 counters and to players in the form of poison counters.)\nAt the beginning of \
the end step, sacrifice this creature.";
const SMOLDERING_BUTCHER: &str =
    "Wither (This deals damage to creatures in the form of -1/-1 counters.)";
const SKITHIRYX: &str = "Flying\nInfect (This creature deals damage to creatures in the form \
of -1/-1 counters and to players in the form of poison counters.)\n{B}: Skithiryx gains haste \
until end of turn.\n{B}{B}: Regenerate Skithiryx.";
const TYPHOID_RATS: &str =
    "Deathtouch (Any amount of damage this deals to a creature is enough to destroy it.)";
const DARKSTEEL_MYR: &str = "Indestructible (Damage and effects that say \"destroy\" don't \
destroy this creature. If its toughness is 0 or less, it still dies.)";
const DOUBLING_SEASON: &str = "If an effect would create one or more tokens under your control, it creates twice that many of those tokens instead.\nIf an effect would put one or more counters on a permanent you control, it puts twice that many of those counters on that permanent instead.";
const VIZIER_OF_REMEDIES: &str =
    "If one or more -1/-1 counters would be put on a creature you control, that many -1/-1 counters minus one are put on it instead.";

fn add_blightsteel(scenario: &mut GameScenario, deathtouch: bool) -> ObjectId {
    let mut b = scenario.add_creature(P0, "Blightsteel Colossus", 11, 11);
    b.from_oracle_text_with_keywords(&["Trample", "Infect", "Indestructible"], BLIGHTSTEEL);
    if deathtouch {
        // A printed grant is Basilisk Collar: "Equipped creature has deathtouch and lifelink."
        b.with_keyword(Keyword::Deathtouch);
    }
    b.id()
}

fn add_bears(scenario: &mut GameScenario, premark: u32) -> ObjectId {
    let mut c = scenario.add_creature(P1, "Grizzly Bears", 2, 2);
    if premark > 0 {
        c.with_damage_marked(premark);
    }
    c.id()
}

/// Cast a free Ram Through with `[source, victim]` targets on a board the
/// caller built, returning the runner (for any resume) and the outcome.
fn cast_ram_through(
    mut scenario: GameScenario,
    source: ObjectId,
    victim: ObjectId,
) -> (GameRunner, CastOutcome) {
    let spell = {
        let mut b = scenario.add_spell_to_hand_from_oracle(P0, "Ram Through", false, RAM_THROUGH);
        b.with_mana_cost(ManaCost::generic(0));
        b.id()
    };
    let mut runner = scenario.build();
    let outcome = runner
        .cast(spell)
        .target_objects(&[source, victim])
        .resolve();
    (runner, outcome)
}

fn new_scenario() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
}

fn minus_counter_adds(events: &[GameEvent], victim: ObjectId) -> Vec<u32> {
    events
        .iter()
        .filter_map(|e| match e {
            GameEvent::CounterAdded {
                object_id,
                counter_type: CounterType::Minus1Minus1,
                count,
                ..
            } if *object_id == victim => Some(*count),
            _ => None,
        })
        .collect()
}

/// `(amount, excess)` of every `DamageDealt` to `target`.
fn dealt_to(events: &[GameEvent], target: TargetRef) -> Vec<(u32, u32)> {
    events
        .iter()
        .filter_map(|e| match e {
            GameEvent::DamageDealt {
                target: t,
                amount,
                excess,
                ..
            } if *t == target => Some((*amount, *excess)),
            _ => None,
        })
        .collect()
}

fn poison(state: &engine::types::game_state::GameState, player: PlayerId) -> u32 {
    state
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.poison_counters)
        .expect("player exists")
}

fn assert_settled(state: &engine::types::game_state::GameState) {
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "the cast must settle to priority, got {:?}",
        state.waiting_for
    );
    assert!(state.stack.is_empty(), "the stack must be empty");
}

/// R1 — CR 120.4a + CR 702.90c/b: 11 infect damage to a 2/2 puts 2 -1/-1
/// counters on it and gives its controller 9 poison.
#[test]
fn infect_redirect_places_only_lethal_counters() {
    let mut scenario = new_scenario();
    let source = add_blightsteel(&mut scenario, false);
    let bears = add_bears(&mut scenario, 0);
    let (_runner, outcome) = cast_ram_through(scenario, source, bears);

    assert_eq!(minus_counter_adds(outcome.events(), bears), vec![2]);
    assert_eq!(poison(outcome.state(), P1), 9);
    outcome.assert_life_delta(P1, 0);
    assert_eq!(
        dealt_to(outcome.events(), TargetRef::Object(bears)),
        vec![(2, 0)]
    );
    assert_eq!(
        dealt_to(outcome.events(), TargetRef::Player(P1)),
        vec![(9, 0)]
    );
    outcome.assert_zone(&[bears], Zone::Graveyard);
    assert_settled(outcome.state());
}

/// R2 — lethal is toughness minus damage already marked: a 2/2 with 1 damage
/// takes 1 counter and the controller 10 poison.
#[test]
fn infect_redirect_accounts_for_marked_damage() {
    let mut scenario = new_scenario();
    let source = add_blightsteel(&mut scenario, false);
    let bears = add_bears(&mut scenario, 1);
    let (_runner, outcome) = cast_ram_through(scenario, source, bears);

    assert_eq!(minus_counter_adds(outcome.events(), bears), vec![1]);
    assert_eq!(poison(outcome.state(), P1), 10);
    assert_eq!(
        dealt_to(outcome.events(), TargetRef::Object(bears)),
        vec![(1, 0)]
    );
    assert_eq!(
        dealt_to(outcome.events(), TargetRef::Player(P1)),
        vec![(10, 0)]
    );
}

/// R3 — CR 702.80a: wither places the lethal 2 counters and the 2 excess
/// is ordinary life loss. No printed creature has both wither and trample, so
/// the harness grants trample, as Primal Frenzy does ("Enchanted creature has trample.").
#[test]
fn wither_redirect_places_only_lethal_counters() {
    let mut scenario = new_scenario();
    let source = {
        let mut c = scenario.add_creature(P0, "Smoldering Butcher", 4, 2);
        c.from_oracle_text_with_keywords(&["Wither"], SMOLDERING_BUTCHER);
        c.with_keyword(Keyword::Trample);
        c.id()
    };
    let bears = add_bears(&mut scenario, 0);
    let (_runner, outcome) = cast_ram_through(scenario, source, bears);

    assert_eq!(minus_counter_adds(outcome.events(), bears), vec![2]);
    outcome.assert_life_delta(P1, -2);
    assert_eq!(poison(outcome.state(), P1), 0);
    outcome.assert_zone(&[bears], Zone::Graveyard);
}

struct OrderingBoard {
    bears: ObjectId,
    doubling: ObjectId,
    vizier: ObjectId,
}

/// Blightsteel Ram Throughs Grizzly Bears while P1 controls Doubling Season and
/// Vizier of Remedies, so the -1/-1 placement pauses on their ordering.
/// Neither modifies counters a player gets, so the excess is 9 poison.
fn pause_on_counter_ordering() -> (GameRunner, CastOutcome, OrderingBoard) {
    let mut scenario = new_scenario();
    let source = add_blightsteel(&mut scenario, false);
    let doubling = scenario
        .add_enchantment_from_oracle(P1, "Doubling Season", DOUBLING_SEASON)
        .id();
    let vizier = scenario
        .add_creature_from_oracle(P1, "Vizier of Remedies", 2, 1, VIZIER_OF_REMEDIES)
        .id();
    let bears = add_bears(&mut scenario, 0);
    let (runner, outcome) = cast_ram_through(scenario, source, bears);
    (
        runner,
        outcome,
        OrderingBoard {
            bears,
            doubling,
            vizier,
        },
    )
}

/// R4 — CR 616.1: counter replacements modify only the lethal portion. With
/// Doubling Season applied before Vizier of Remedies, 2 counters become 3.
#[test]
fn infect_redirect_pause_resume_modifies_only_lethal_counters() {
    let (
        mut runner,
        outcome,
        OrderingBoard {
            bears,
            doubling,
            vizier,
        },
    ) = pause_on_counter_ordering();

    let WaitingFor::ReplacementChoice {
        player,
        kind,
        candidates,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected a counter-ordering prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P1);
    assert_eq!(kind, ReplacementChoiceKind::Order);
    assert_eq!(candidates.len(), 2, "{candidates:?}");
    assert!(candidates.iter().any(|c| c.source_id == vizier));
    let index = candidates
        .iter()
        .position(|c| c.source_id == doubling)
        .expect("Doubling Season is a candidate");
    let mut events = outcome.events().to_vec();
    events.extend(
        runner
            .act(GameAction::ChooseReplacement { index })
            .expect("ordering pick accepted")
            .events,
    );
    runner.advance_until_stack_empty();

    assert_eq!(minus_counter_adds(&events, bears), vec![3]);
    assert_eq!(dealt_to(&events, TargetRef::Player(P1)), vec![(9, 0)]);
    assert_eq!(poison(runner.state(), P1), 9);
    assert_eq!(runner.state().objects[&bears].zone, Zone::Graveyard);
    assert_settled(runner.state());
}

/// R5 — paired negative (no trample, no redirect): the full 4 infect damage
/// becomes counters, and CR 120.10 excess counts damage already marked.
#[test]
fn infect_without_trample_places_all_counters_and_reports_excess() {
    let mut scenario = new_scenario();
    let source = {
        let mut c = scenario.add_creature(P0, "Skithiryx, the Blight Dragon", 4, 4);
        c.from_oracle_text_with_keywords(&["Flying", "Infect"], SKITHIRYX);
        c.id()
    };
    let bears = add_bears(&mut scenario, 1);
    let (_runner, outcome) = cast_ram_through(scenario, source, bears);

    assert_eq!(minus_counter_adds(outcome.events(), bears), vec![4]);
    assert_eq!(poison(outcome.state(), P1), 0);
    outcome.assert_life_delta(P1, 0);
    assert_eq!(
        dealt_to(outcome.events(), TargetRef::Object(bears)),
        vec![(4, 3)]
    );
}

/// R6 — CR 120.4a deathtouch: 1 damage is lethal, so 5 infect damage puts 1
/// counter on the 2/2 (which then dies to CR 704.5h) and gives 4 poison.
#[test]
fn deathtouch_infect_redirect_places_one_counter() {
    let mut scenario = new_scenario();
    let source = {
        let mut c = scenario.add_creature(P0, "Putrefax", 5, 3);
        c.from_oracle_text_with_keywords(&["Haste", "Infect", "Trample"], PUTREFAX);
        // A printed grant is Basilisk Collar: "Equipped creature has deathtouch and lifelink."
        c.with_keyword(Keyword::Deathtouch);
        c.id()
    };
    let bears = add_bears(&mut scenario, 0);
    let (_runner, outcome) = cast_ram_through(scenario, source, bears);

    assert_eq!(minus_counter_adds(outcome.events(), bears), vec![1]);
    assert_eq!(poison(outcome.state(), P1), 4);
    outcome.assert_zone(&[bears], Zone::Graveyard);
}

/// R7 — CR 120.4a: deathtouch lethal stays 1 even with damage already marked.
#[test]
fn deathtouch_infect_redirect_ignores_marked_damage() {
    let mut scenario = new_scenario();
    let source = add_blightsteel(&mut scenario, true);
    let bears = add_bears(&mut scenario, 1);
    let (_runner, outcome) = cast_ram_through(scenario, source, bears);

    assert_eq!(minus_counter_adds(outcome.events(), bears), vec![1]);
    assert_eq!(poison(outcome.state(), P1), 10);
}

/// R8 — CR 120.4a: 1 deathtouch damage is never excess, so an indestructible
/// creature with damage marked takes it and nothing is redirected. No printed
/// creature has both deathtouch and trample, so the harness grants trample, as
/// Primal Frenzy does ("Enchanted creature has trample.").
#[test]
fn deathtouch_single_damage_is_never_excess() {
    let mut scenario = new_scenario();
    let source = {
        let mut c = scenario.add_creature(P0, "Typhoid Rats", 1, 1);
        c.from_oracle_text_with_keywords(&["Deathtouch"], TYPHOID_RATS);
        c.with_keyword(Keyword::Trample);
        c.id()
    };
    let myr = {
        let mut c = scenario.add_creature(P1, "Darksteel Myr", 0, 1);
        c.from_oracle_text_with_keywords(&["Indestructible"], DARKSTEEL_MYR);
        c.with_damage_marked(1);
        c.id()
    };
    let (_runner, outcome) = cast_ram_through(scenario, source, myr);

    assert_eq!(outcome.state().objects[&myr].damage_marked, 2);
    outcome.assert_life_delta(P1, 0);
    assert!(dealt_to(outcome.events(), TargetRef::Player(P1)).is_empty());
}

fn restore(json: &str) -> Result<GameState, String> {
    serde_json::from_str::<PersistedGameState>(json)
        .map_err(|error| error.to_string())?
        .prepare_for_restore(PersistedRestoreFinalization::DeferUntilRehydrated)
        .map_err(|error| error.to_string())?
        .finalize_after_rehydration(|_| Ok(()))
        .map_err(|error| error.to_string())
}

/// Rewrites every paused counter stash to the previous writer's shape
/// (`loyalty_before`/`defense_before`, no `excess`); returns how many it rewrote.
fn write_previous_counter_stash(value: &mut serde_json::Value) -> usize {
    match value {
        serde_json::Value::Object(object) => {
            let mut rewritten = 0;
            if let Some(serde_json::Value::Object(stash)) =
                object.get_mut("pending_damage_post_counter")
            {
                assert!(stash.remove("excess").is_some(), "{stash:?}");
                stash.insert("loyalty_before".into(), serde_json::Value::Null);
                stash.insert("defense_before".into(), serde_json::Value::Null);
                rewritten += 1;
            }
            rewritten
                + object
                    .values_mut()
                    .map(write_previous_counter_stash)
                    .sum::<usize>()
        }
        serde_json::Value::Array(values) => {
            values.iter_mut().map(write_previous_counter_stash).sum()
        }
        _ => 0,
    }
}

/// CR 120.4a: the paused stash owes 9 excess poison, so a save written before
/// it carried `excess` is refused rather than resumed with no redirect; a
/// current save restores and still redirects the excess.
#[test]
fn paused_counter_stash_without_excess_is_refused_at_restore() {
    for trusted in [false, true] {
        let (
            runner,
            _outcome,
            OrderingBoard {
                bears, doubling, ..
            },
        ) = pause_on_counter_ordering();
        assert_eq!(
            runner
                .state()
                .pending_damage_post_counter
                .as_ref()
                .map(|stash| stash.excess),
            Some(9),
            "the pause stashes the excess"
        );
        let persisted = if trusted {
            PersistedGameState::capture(runner.state().clone())
        } else {
            PersistedGameState::Raw(Box::new(runner.state().clone()))
        };
        let wire = serde_json::to_value(&persisted).expect("the pause serializes");

        let mut resumed =
            GameRunner::from_state(restore(&wire.to_string()).expect("a current save restores"));
        let WaitingFor::ReplacementChoice { candidates, .. } = resumed.state().waiting_for.clone()
        else {
            panic!("trusted {trusted}: {:?}", resumed.state().waiting_for);
        };
        let index = candidates
            .iter()
            .position(|c| c.source_id == doubling)
            .expect("Doubling Season is a candidate");
        let events = resumed
            .act(GameAction::ChooseReplacement { index })
            .expect("ordering pick accepted")
            .events;
        resumed.advance_until_stack_empty();
        assert_eq!(
            minus_counter_adds(&events, bears),
            vec![3],
            "trusted {trusted}"
        );
        assert_eq!(poison(resumed.state(), P1), 9, "trusted {trusted}");
        assert_settled(resumed.state());

        let mut previous = wire;
        assert_eq!(write_previous_counter_stash(&mut previous), 1);
        let error = restore(&previous.to_string()).expect_err("a stash without excess is refused");
        assert!(
            error.contains("missing field `excess`"),
            "trusted {trusted}: {error}"
        );
    }
}
