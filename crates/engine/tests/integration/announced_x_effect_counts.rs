//! Regression for phase-rs/phase#7729: Doppelgang resolved and created nothing, and
//! Devastating Onslaught cast with X=4 targeting Aerid Konstrari created no copies.

use std::collections::BTreeMap;

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use super::rules::run_combat;

const DEVASTATING_ONSLAUGHT: &str = "Create X tokens that are copies of target artifact or \
creature you control. Those tokens gain haste until end of turn. Sacrifice them at the \
beginning of the next end step.";

const DOPPELGANG: &str = "For each of X target permanents, create X tokens that are copies of \
that permanent.";

const AGGRESSIVE_BIOMANCY: &str = "Create X tokens that are copies of target creature you \
control, except they have \"When this token enters, it fights up to one target creature you \
don't control.\"";

const FOR_THE_COMMON_GOOD: &str = "Create X tokens that are copies of target token you control. \
Then tokens you control gain indestructible until your next turn. You gain 1 life for each \
token you control.";

const THE_GOOD_GAMERS: &str = "Establishing Shot — At the beginning of your first upkeep, if \
The Good Gamers is in the command zone, it deals 2 damage to each opponent.\nWhen The Good \
Gamers enters, create X tokens that are copies of it. Give this creature and the tokens unique \
names.\nAll damage dealt by this creature to an opponent counts as commander damage for your \
commander (including noncombat damage and even if this isn't your commander).";

const AERID_KONSTRARI: &str = "Flying\nWhen Aerid Konstrari enters or dies, create a Heartwood \
token. (It's a red and green artifact with \"{T}: Add {R} or {G}.\")\n{6}: Create a Heartwood \
token. Then Aerid Konstrari gets +X/+0 until end of turn, where X is the number of artifacts \
you control.";

const FLOCK_OF_RABID_SHEEP: &str = "Flip X coins. For each flip you win, create a 2/2 green \
Sheep creature token named Rabid Sheep.";

const ATTEMPTED_MURDER: &str = "Choose target creature. Roll X six-sided dice. For each even \
result, put two -1/-1 counters on that creature. For each odd result, create a 1/2 blue Bird \
creature token with flying named Storm Crow.";

const CLOWN_CAR: &str = "When this Vehicle enters, roll X six-sided dice. For each odd result, \
create a 1/1 white Clown Robot artifact creature token. For each even result, put a +1/+1 \
counter on this Vehicle.\nCrew 2";

const NACATL_WAR_PRIDE: &str = "This creature must be blocked by exactly one creature if able.\n\
Whenever this creature attacks, create X tokens that are copies of it and that are tapped and \
attacking, where X is the number of creatures defending player controls. Exile the tokens at \
the beginning of the next end step.";

const RIONYA_FIRE_DANCER: &str = "At the beginning of combat on your turn, create X tokens that \
are copies of another target creature you control, where X is one plus the number of instant \
and sorcery spells you've cast this turn. They gain haste. Exile them at the beginning of the \
next end step.";

const ADAPTIVE_AUTOMATON: &str =
    "As this creature enters, choose a creature type.\nThis creature is \
the chosen type in addition to its other types.\nOther creatures you control of the chosen type \
get +1/+1.";

const QUICKSILVER_GARGANTUAN: &str = "You may have this creature enter as a copy of any creature \
on the battlefield, except it's 7/7.";

const FAITHFUL_WATCHDOG: &str = "Vigilance\nThis creature enters with three +1/+1 counters on it.";

const HARDENED_SCALES: &str = "If one or more +1/+1 counters would be put on a creature you \
control, that many plus one +1/+1 counters are put on it instead.";

const CORPSEJACK_MENACE: &str = "If one or more +1/+1 counters would be put on a creature you \
control, twice that many +1/+1 counters are put on it instead.";

const OPT: &str = "Scry 1. (Look at the top card of your library. You may put that card on the \
bottom.)\nDraw a card.";

const FIRE_LORD_SOZIN: &str = "Menace, firebending 3 (Whenever this creature attacks, add \
{R}{R}{R}. This mana lasts until end of combat.)\nWhenever Fire Lord Sozin deals combat damage \
to a player, you may pay {X}. When you do, put any number of target creature cards with total \
mana value X or less from that player's graveyard onto the battlefield under your control.";

fn cost(shards: Vec<ManaCostShard>) -> ManaCost {
    ManaCost::Cost { shards, generic: 0 }
}

fn add_mana(runner: &mut GameRunner, color: ManaType, count: usize) {
    for _ in 0..count {
        let unit = ManaUnit::new(color, ObjectId(0), false, vec![]);
        runner.state_mut().players[0].mana_pool.add(unit);
    }
}

/// Battlefield permanents P0 controls, keyed by `name` (plus `(token)` for tokens).
fn p0_permanents(runner: &GameRunner) -> BTreeMap<String, usize> {
    let state = runner.state();
    let mut counts = BTreeMap::new();
    for id in &state.battlefield {
        let object = &state.objects[id];
        if object.controller == P0 {
            let key = format!(
                "{}{}",
                object.name,
                if object.is_token { "(token)" } else { "" }
            );
            *counts.entry(key).or_insert(0) += 1;
        }
    }
    counts
}

fn count_of(runner: &GameRunner, key: &str) -> usize {
    p0_permanents(runner).get(key).copied().unwrap_or(0)
}

fn count_events(events: &[GameEvent], pred: impl Fn(&GameEvent) -> bool) -> usize {
    events.iter().filter(|event| pred(event)).count()
}

fn devastating_onslaught_scenario(
    target: impl FnOnce(&mut GameScenario) -> ObjectId,
) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    let target = target(&mut scenario);
    let mut builder = scenario.add_spell_to_hand_from_oracle(
        P0,
        "Devastating Onslaught",
        false,
        DEVASTATING_ONSLAUGHT,
    );
    builder.with_mana_cost(cost(vec![
        ManaCostShard::X,
        ManaCostShard::X,
        ManaCostShard::Red,
    ]));
    let spell = builder.id();
    let mut runner = scenario.build();
    add_mana(&mut runner, ManaType::Red, 9);
    (runner, spell, target)
}

#[test]
fn devastating_onslaught_creates_x_copies_then_sacrifices_all_of_them() {
    let (mut runner, spell, bear) = devastating_onslaught_scenario(|scenario| {
        scenario.add_creature(P0, "Grizzly Bears", 2, 2).id()
    });

    let outcome = runner.cast(spell).x(4).target_object(bear).resolve();

    assert_eq!(
        count_of(&runner, "Grizzly Bears(token)"),
        4,
        "X=4 must create four copies (positive reach for the end-step assertions below)"
    );
    assert_eq!(count_of(&runner, "Grizzly Bears"), 1);
    assert_eq!(outcome.zone_of(spell), Zone::Graveyard);

    runner.advance_to_end_step();
    runner.advance_until_stack_empty();

    assert_eq!(
        count_of(&runner, "Grizzly Bears(token)"),
        0,
        "the end-step sacrifice takes every copy"
    );
    assert_eq!(count_of(&runner, "Grizzly Bears"), 1);
}

#[test]
fn doppelgang_creates_x_copies_of_each_of_x_targets() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let alpha = scenario.add_creature(P0, "Alpha Bear", 2, 2).id();
    let beta = scenario.add_creature(P0, "Beta Bear", 3, 3).id();
    let mut builder = scenario.add_spell_to_hand_from_oracle(P0, "Doppelgang", false, DOPPELGANG);
    builder.with_mana_cost(cost(vec![
        ManaCostShard::X,
        ManaCostShard::X,
        ManaCostShard::X,
        ManaCostShard::Green,
        ManaCostShard::Blue,
    ]));
    let spell = builder.id();
    let mut runner = scenario.build();
    add_mana(&mut runner, ManaType::Green, 7);
    add_mana(&mut runner, ManaType::Blue, 1);

    let _ = runner
        .cast(spell)
        .x(2)
        .target_objects(&[alpha, beta])
        .resolve();

    assert_eq!(count_of(&runner, "Alpha Bear(token)"), 2);
    assert_eq!(count_of(&runner, "Beta Bear(token)"), 2);
}

#[test]
fn aggressive_biomancy_creates_x_copies_of_the_target() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let mut builder = scenario.add_spell_to_hand_from_oracle(
        P0,
        "Aggressive Biomancy",
        false,
        AGGRESSIVE_BIOMANCY,
    );
    builder.with_mana_cost(cost(vec![
        ManaCostShard::X,
        ManaCostShard::X,
        ManaCostShard::Green,
        ManaCostShard::Blue,
    ]));
    let spell = builder.id();
    let mut runner = scenario.build();
    add_mana(&mut runner, ManaType::Green, 7);
    add_mana(&mut runner, ManaType::Blue, 1);

    let _ = runner.cast(spell).x(3).target_object(bear).resolve();

    assert_eq!(count_of(&runner, "Grizzly Bears(token)"), 3);
}

#[test]
fn for_the_common_good_creates_x_token_copies_and_gains_life_per_token() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let goblin = scenario.add_creature(P0, "Goblin", 1, 1).id();
    let mut builder = scenario.add_spell_to_hand_from_oracle(
        P0,
        "For the Common Good",
        false,
        FOR_THE_COMMON_GOOD,
    );
    builder.with_mana_cost(cost(vec![
        ManaCostShard::X,
        ManaCostShard::X,
        ManaCostShard::Green,
    ]));
    let spell = builder.id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&goblin)
        .expect("goblin exists")
        .is_token = true;
    add_mana(&mut runner, ManaType::Green, 5);

    let outcome = runner.cast(spell).x(2).target_object(goblin).resolve();

    assert_eq!(
        count_of(&runner, "Goblin(token)"),
        3,
        "the original plus X=2 copies"
    );
    outcome.assert_life_delta(P0, 3);
}

#[test]
fn the_good_gamers_enters_and_creates_x_copies_of_itself() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut builder =
        scenario.add_creature_to_hand_from_oracle(P0, "The Good Gamers", 3, 3, THE_GOOD_GAMERS);
    builder.with_mana_cost(cost(vec![
        ManaCostShard::X,
        ManaCostShard::Blue,
        ManaCostShard::Red,
        ManaCostShard::White,
    ]));
    let spell = builder.id();
    let mut runner = scenario.build();
    add_mana(&mut runner, ManaType::Blue, 1);
    add_mana(&mut runner, ManaType::Red, 1);
    add_mana(&mut runner, ManaType::White, 3);

    let _ = runner.cast(spell).x(2).resolve();

    assert_eq!(count_of(&runner, "The Good Gamers(token)"), 2);
}

/// The Aerid Konstrari scenario from the bug report: X=4 copies of a legendary
/// creature, the legend rule (CR 704.5j) keeps one, and each of the four copies
/// that entered and each of the four that died leaves a Heartwood (CR 603.6a +
/// CR 700.4).
fn aerid_scenario_at_legend_choice() -> (GameRunner, ObjectId, ObjectId, Vec<ObjectId>) {
    let (mut runner, spell, aerid) = devastating_onslaught_scenario(|scenario| {
        scenario
            .add_creature(P0, "Aerid Konstrari", 4, 4)
            .as_legendary()
            .with_subtypes(vec!["Elder", "Sphinx"])
            .from_oracle_text(AERID_KONSTRARI)
            .id()
    });
    let _ = runner.cast(spell).x(4).target_object(aerid).resolve();
    let candidates = match runner.state().waiting_for.clone() {
        WaitingFor::ChooseLegend { candidates, .. } => candidates,
        other => panic!("expected ChooseLegend after four legendary copies, got {other:?}"),
    };
    (runner, spell, aerid, candidates)
}

#[test]
fn devastating_onslaught_on_a_legend_keeps_eight_heartwood_through_the_end_step() {
    let (mut runner, _spell, aerid, candidates) = aerid_scenario_at_legend_choice();
    assert_eq!(candidates.len(), 5, "the original plus four copies");
    assert!(candidates.contains(&aerid));

    runner
        .act(GameAction::ChooseLegend { keep: aerid })
        .expect("keeping the original is a legal legend choice");
    runner.advance_until_stack_empty();

    assert_eq!(count_of(&runner, "Aerid Konstrari"), 1);
    assert_eq!(count_of(&runner, "Aerid Konstrari(token)"), 0);
    assert_eq!(
        count_of(&runner, "Heartwood(token)"),
        8,
        "four enters triggers and four dies triggers"
    );

    runner.advance_to_end_step();
    runner.advance_until_stack_empty();

    assert_eq!(
        count_of(&runner, "Aerid Konstrari"),
        1,
        "every copy is already gone, so nothing named 'them' is left to sacrifice"
    );
    assert_eq!(
        count_of(&runner, "Heartwood(token)"),
        8,
        "the delayed sacrifice must not fall through to a later token producer's tokens"
    );
}

#[test]
fn devastating_onslaught_on_a_legend_sacrifices_a_kept_copy_at_end_step() {
    let (mut runner, _spell, aerid, candidates) = aerid_scenario_at_legend_choice();
    let copy = *candidates
        .iter()
        .find(|candidate| **candidate != aerid)
        .expect("a token copy is among the candidates");

    runner
        .act(GameAction::ChooseLegend { keep: copy })
        .expect("keeping a copy is a legal legend choice");
    runner.advance_until_stack_empty();
    assert_eq!(
        count_of(&runner, "Aerid Konstrari(token)"),
        1,
        "the kept copy is on the battlefield before the end step"
    );

    runner.advance_to_end_step();
    runner.advance_until_stack_empty();

    assert_eq!(count_of(&runner, "Aerid Konstrari(token)"), 0);
    assert_eq!(count_of(&runner, "Aerid Konstrari"), 0);
    assert_eq!(
        count_of(&runner, "Heartwood(token)"),
        9,
        "four enters, four legend-rule deaths, and the sacrificed copy's own death"
    );
}

#[test]
fn flock_of_rabid_sheep_flips_x_coins() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut builder = scenario.add_spell_to_hand_from_oracle(
        P0,
        "Flock of Rabid Sheep",
        false,
        FLOCK_OF_RABID_SHEEP,
    );
    builder.with_mana_cost(cost(vec![
        ManaCostShard::X,
        ManaCostShard::Green,
        ManaCostShard::Green,
    ]));
    let spell = builder.id();
    let mut runner = scenario.build();
    add_mana(&mut runner, ManaType::Green, 5);

    let outcome = runner.cast(spell).x(3).resolve();

    let flips = count_events(outcome.events(), |e| {
        matches!(e, GameEvent::CoinFlipped { .. })
    });
    let wins = count_events(outcome.events(), |e| {
        matches!(e, GameEvent::CoinFlipped { won: true, .. })
    });
    assert_eq!(flips, 3);
    assert_eq!(count_of(&runner, "Rabid Sheep(token)"), wins);
}

#[test]
fn attempted_murder_rolls_x_dice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let victim = scenario.add_creature(P1, "Victim", 9, 9).id();
    let mut builder =
        scenario.add_spell_to_hand_from_oracle(P0, "Attempted Murder", false, ATTEMPTED_MURDER);
    builder.with_mana_cost(cost(vec![
        ManaCostShard::X,
        ManaCostShard::Black,
        ManaCostShard::Black,
    ]));
    let spell = builder.id();
    let mut runner = scenario.build();
    add_mana(&mut runner, ManaType::Black, 5);

    let outcome = runner.cast(spell).x(3).target_object(victim).resolve();

    let rolls = count_events(outcome.events(), |e| {
        matches!(e, GameEvent::DieRolled { .. })
    });
    assert_eq!(rolls, 3);
}

#[test]
fn clown_car_enters_and_rolls_x_dice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut builder = scenario.add_creature_to_hand_from_oracle(P0, "Clown Car", 3, 3, CLOWN_CAR);
    builder.with_mana_cost(cost(vec![ManaCostShard::X]));
    builder.with_subtypes(vec!["Vehicle"]);
    let spell = builder.id();
    let mut runner = scenario.build();
    add_mana(&mut runner, ManaType::Red, 3);

    let outcome = runner.cast(spell).x(3).resolve();

    let rolls = count_events(outcome.events(), |e| {
        matches!(e, GameEvent::DieRolled { .. })
    });
    assert_eq!(rolls, 3);
    assert_eq!(outcome.zone_of(spell), Zone::Battlefield);
}

#[test]
fn nacatl_war_pride_copies_track_the_defending_players_creature_count() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let nacatl = scenario
        .add_creature_from_oracle(P0, "Nacatl War-Pride", 3, 3, NACATL_WAR_PRIDE)
        .id();
    for name in ["Blocker A", "Blocker B", "Blocker C"] {
        scenario.add_creature(P1, name, 1, 1);
    }
    let mut runner = scenario.build();

    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(nacatl, engine::game::combat::AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("declaring Nacatl War-Pride as an attacker succeeds");
    runner.advance_until_stack_empty();

    assert_eq!(
        count_of(&runner, "Nacatl War-Pride(token)"),
        3,
        "X is the number of creatures P1 controls"
    );
}

#[test]
fn rionya_copies_track_the_instants_and_sorceries_cast_this_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Rionya, Fire Dancer", 3, 3, RIONYA_FIRE_DANCER);
    let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    scenario.with_library_top(P0, &["Library A", "Library B", "Library C", "Library D"]);
    let opt_a = scenario
        .add_spell_to_hand_from_oracle(P0, "Opt", true, OPT)
        .with_mana_cost(cost(vec![ManaCostShard::Blue]))
        .id();
    let opt_b = scenario
        .add_spell_to_hand_from_oracle(P0, "Opt", true, OPT)
        .with_mana_cost(cost(vec![ManaCostShard::Blue]))
        .id();
    let mut runner = scenario.build();
    add_mana(&mut runner, ManaType::Blue, 2);

    let _ = runner.cast(opt_a).resolve();
    let _ = runner.cast(opt_b).resolve();
    runner.advance_to_phase(Phase::BeginCombat);
    if matches!(
        runner.state().waiting_for,
        WaitingFor::TriggerTargetSelection { .. }
    ) {
        runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Object(bear)],
            })
            .expect("Grizzly Bears is a legal target for Rionya's trigger");
    }
    runner.advance_until_stack_empty();

    assert_eq!(
        count_of(&runner, "Grizzly Bears(token)"),
        3,
        "one plus the two instants cast this turn"
    );
}

/// CR 603.12: the reflexive "When you do" trigger carries the X paid for "you may
/// pay {X}", and that X caps the total mana value of the chosen graveyard cards.
#[test]
fn fire_lord_sozin_reflexive_cap_reads_the_x_that_was_paid() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let sozin = scenario
        .add_creature_from_oracle(P0, "Fire Lord Sozin", 5, 5, FIRE_LORD_SOZIN)
        .id();
    for _ in 0..3 {
        scenario.add_basic_land(P0, ManaColor::Red);
    }
    let mut graveyard = Vec::new();
    for mana_value in [1u32, 2, 4] {
        let mut builder =
            scenario.add_creature_to_graveyard(P1, &format!("Grave {mana_value}"), 1, 1);
        builder.with_mana_cost(ManaCost::generic(mana_value));
        graveyard.push(builder.id());
    }
    let mut runner = scenario.build();

    run_combat(&mut runner, vec![sozin], vec![]);
    let (grave_1, grave_2, grave_4) = (graveyard[0], graveyard[1], graveyard[2]);

    let mut reached_target_selection = false;
    for _ in 0..30 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accepting the optional payment");
            }
            WaitingFor::PayAmountChoice { .. } => {
                runner
                    .act(GameAction::SubmitPayAmount { amount: 3 })
                    .expect("paying X=3");
            }
            WaitingFor::TriggerTargetSelection { .. } => {
                reached_target_selection = true;
                let over_cap = runner.act(GameAction::SelectTargets {
                    targets: vec![TargetRef::Object(grave_4)],
                });
                assert!(
                    over_cap.is_err(),
                    "total mana value 4 exceeds the X=3 that was paid"
                );
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(grave_1), TargetRef::Object(grave_2)],
                    })
                    .expect("total mana value 3 is within the X=3 that was paid");
                break;
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .expect("ordering triggers");
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority toward the reflexive trigger");
            }
            other => panic!("unexpected prompt before the reflexive target selection: {other:?}"),
        }
    }
    assert!(
        reached_target_selection,
        "the reflexive trigger must reach target selection"
    );
    runner.advance_until_stack_empty();

    let state = runner.state();
    for returned in [grave_1, grave_2] {
        assert_eq!(state.objects[&returned].zone, Zone::Battlefield);
        assert_eq!(
            state.objects[&returned].controller, P0,
            "put onto the battlefield under Sozin's controller"
        );
    }
    assert_eq!(state.objects[&grave_4].zone, Zone::Graveyard);
}

/// Answers each prompt a copy's entry raises until none is pending, and returns how
/// many were answered. `copy_target` is the permanent an enter-as-a-copy choice picks.
fn answer_copy_entry_prompts(runner: &mut GameRunner, copy_target: Option<ObjectId>) -> usize {
    let mut answered = 0;
    loop {
        let action = match runner.state().waiting_for.clone() {
            WaitingFor::NamedChoice { options, .. } => GameAction::ChooseOption {
                choice: options
                    .first()
                    .cloned()
                    .expect("a creature-type choice offers options"),
            },
            WaitingFor::ReplacementChoice { .. } => GameAction::ChooseReplacement { index: 0 },
            WaitingFor::CopyTargetChoice { valid_targets, .. } => GameAction::ChooseTarget {
                target: Some(TargetRef::Object(
                    copy_target
                        .filter(|target| valid_targets.contains(target))
                        .expect("the copy choice offers the planted permanent"),
                )),
            },
            _ => return answered,
        };
        runner
            .act(action)
            .expect("the entry prompt accepts this answer");
        answered += 1;
        assert!(answered < 20, "entry prompts must not repeat without end");
    }
}

/// The tokens P0 controls on the battlefield.
fn p0_token_ids(runner: &GameRunner) -> Vec<ObjectId> {
    let state = runner.state();
    state
        .battlefield
        .iter()
        .copied()
        .filter(|id| state.objects[id].is_token && state.objects[id].controller == P0)
        .collect()
}

/// With both copies on the battlefield: both
/// have haste, the resolution stack is empty, and one delayed trigger names and pins
/// exactly the two copies (CR 603.7c).
fn assert_copies_hasted_and_named_by_one_delayed_trigger(runner: &GameRunner) {
    let state = runner.state();
    let copies = p0_token_ids(runner);
    assert_eq!(copies.len(), 2, "X=2 creates two copies");
    assert!(
        state.resolution_stack.is_empty(),
        "the spell's instructions finish before any priority pass, left {:?}",
        state
            .resolution_stack
            .iter()
            .map(|frame| frame.kind())
            .collect::<Vec<_>>()
    );
    for id in &copies {
        assert!(
            state.objects[id].has_keyword(&Keyword::Haste),
            "\"those tokens gain haste\" reaches every copy"
        );
    }
    assert_eq!(state.delayed_triggers.len(), 1);
    let delayed = &state.delayed_triggers[0].ability;
    let mut named: Vec<ObjectId> = delayed
        .targets
        .iter()
        .map(|target| match target {
            TargetRef::Object(id) => *id,
            other => panic!("the delayed sacrifice names objects, got {other:?}"),
        })
        .collect();
    named.sort();
    let mut expected = copies;
    expected.sort();
    assert_eq!(named, expected, "\"sacrifice them\" names every copy");
    assert_eq!(delayed.target_incarnations.len(), 2);
}

fn assert_copies_are_sacrificed_and_the_original_stays(
    runner: &mut GameRunner,
    original: ObjectId,
) {
    runner.advance_to_end_step();
    runner.advance_until_stack_empty();
    assert_eq!(
        p0_token_ids(runner).len(),
        0,
        "the end-step sacrifice takes every copy"
    );
    assert!(runner.state().battlefield.contains(&original));
}

#[test]
fn devastating_onslaught_on_a_creature_with_an_as_enters_choice_hastes_and_sacrifices_every_copy() {
    let (mut runner, spell, automaton) = devastating_onslaught_scenario(|scenario| {
        scenario
            .add_creature_from_oracle(P0, "Adaptive Automaton", 2, 2, ADAPTIVE_AUTOMATON)
            .id()
    });

    let _ = runner.cast(spell).x(2).target_object(automaton).resolve();
    let prompts = answer_copy_entry_prompts(&mut runner, None);

    assert_eq!(prompts, 2, "each copy asks for its creature type");
    assert_copies_hasted_and_named_by_one_delayed_trigger(&runner);
    assert_copies_are_sacrificed_and_the_original_stays(&mut runner, automaton);
}

#[test]
fn devastating_onslaught_on_an_enter_as_a_copy_creature_hastes_and_sacrifices_every_copy() {
    let (mut runner, spell, gargantuan) = devastating_onslaught_scenario(|scenario| {
        scenario.add_creature(P0, "Grizzly Bears", 2, 2);
        scenario
            .add_creature_from_oracle(P0, "Quicksilver Gargantuan", 7, 7, QUICKSILVER_GARGANTUAN)
            .id()
    });
    let bears = runner
        .state()
        .battlefield
        .iter()
        .copied()
        .find(|id| runner.state().objects[id].name == "Grizzly Bears")
        .expect("Grizzly Bears is planted on the battlefield");

    let _ = runner.cast(spell).x(2).target_object(gargantuan).resolve();
    let prompts = answer_copy_entry_prompts(&mut runner, Some(bears));

    assert_eq!(
        prompts, 4,
        "each copy asks whether to enter as a copy, then which permanent"
    );
    assert_copies_hasted_and_named_by_one_delayed_trigger(&runner);
    assert_copies_are_sacrificed_and_the_original_stays(&mut runner, gargantuan);
}

#[test]
fn devastating_onslaught_on_a_creature_entering_with_counters_under_two_replacements_hastes_and_sacrifices_every_copy(
) {
    let (mut runner, spell, watchdog) = devastating_onslaught_scenario(|scenario| {
        scenario.add_enchantment_from_oracle(P0, "Hardened Scales", HARDENED_SCALES);
        scenario.add_creature_from_oracle(P0, "Corpsejack Menace", 4, 4, CORPSEJACK_MENACE);
        let watchdog = scenario
            .add_creature_from_oracle(P0, "Faithful Watchdog", 0, 0, FAITHFUL_WATCHDOG)
            .id();
        scenario.with_counter(watchdog, CounterType::Plus1Plus1, 3);
        watchdog
    });

    let _ = runner.cast(spell).x(2).target_object(watchdog).resolve();
    let prompts = answer_copy_entry_prompts(&mut runner, None);

    assert_eq!(
        prompts, 2,
        "each copy asks which counter replacement applies first (CR 616.1)"
    );
    assert_copies_hasted_and_named_by_one_delayed_trigger(&runner);
    assert_copies_are_sacrificed_and_the_original_stays(&mut runner, watchdog);
}
