//! Runtime regression tests for Darksteel Angel (issue #10981):
//!
//! "Flying, indestructible\nYou can't lose the game and your opponents can't
//! win the game.\nCreatures you control can't have -1/-1 counters put on
//! them."
//!
//! CR 614.6 + CR 122.1 + CR 614.17: the third line is an AddCounter+Prevent
//! replacement scoped to creatures its controller controls on the
//! battlefield, matching only -1/-1 counters. Rows:
//!
//! (R0) Solemnity × wither: documents the adjacent wither-bypass defect and
//!      isolates the engine half of the fix (Solemnity parses at base).
//! (R3) Wither combat damage to a controlled creature puts no counters on
//!      it, while wither damage to an opponent's creature still does
//!      (population scope) and ordinary damage is still marked (the damage
//!      step ran — the anti-vacuity guard). A pre-installed counter stays
//!      (prevention removes nothing).
//! (R3b) An opponent's Angel does not protect your creatures (source-
//!      controller binding).
//! (R4) +1/+1 counters still land on controlled creatures (counter-type
//!      scope), driven by casting Servant of the Scale.
//! (R5) Persist re-entry under the Angel gets no counter (entry path),
//!      driven by casting Kitchen Finks.
//! (R6) Wither flows through non-prevention replacements (Winding
//!      Constrictor adds one).
//! (R7) Damage is still dealt (lifelink gains life on wither damage).
//! (R8) Counter costs are unpayable under the Angel (Aboroth mirror).

use engine::game::combat::AttackTarget;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, Outcome, P0, P1};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{parse_mtgjson_mana_cost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

/// Darksteel Angel's VERBATIM Scryfall Oracle text.
const DARKSTEEL_ANGEL_ORACLE: &str = "Flying, indestructible\nYou can't lose the game and your opponents can't win the game.\nCreatures you control can't have -1/-1 counters put on them.";

/// Servant of the Scale's VERBATIM Oracle text ({G} 0/0).
const SERVANT_OF_THE_SCALE_ORACLE: &str = "This creature enters with a +1/+1 counter on it.\nWhen this creature dies, put X +1/+1 counters on target creature you control, where X is the number of +1/+1 counters on this creature.";

/// Solemnity's VERBATIM Oracle text.
const SOLEMNITY_ORACLE: &str = "Players can't get counters.\nCounters can't be put on artifacts, creatures, enchantments, or lands.";

/// Aboroth's VERBATIM Oracle text (9/9; note the em dash).
const ABOROTH_ORACLE: &str = "Cumulative upkeep—Put a -1/-1 counter on this creature. (At the beginning of your upkeep, put an age counter on this permanent, then sacrifice it unless you pay its upkeep cost for each age counter on it.)";

/// Kitchen Finks' VERBATIM Oracle text ({1}{G/W}{G/W} 3/2).
const KITCHEN_FINKS_ORACLE: &str = "When this creature enters, you gain 2 life.\nPersist (When this creature dies, if it had no -1/-1 counters on it, return it to the battlefield under its owner's control with a -1/-1 counter on it.)";

/// Winding Constrictor's VERBATIM Oracle text ({B}{G} 2/3).
///
/// Leg 2 (player counters) is an honest coverage gap and uninvolved in R6 —
/// no player counters occur there.
const WINDING_CONSTRICTOR_ORACLE: &str = "If one or more counters would be put on an artifact or creature you control, that many plus one of each of those kinds of counters are put on that permanent instead.\nIf you would get one or more counters, you get that many plus one of each of those kinds of counters instead.";

/// Corrosive Mentor's VERBATIM Oracle text ({2}{B} 1/3).
const CORROSIVE_MENTOR_ORACLE: &str =
    "Black creatures you control have wither. (They deal damage to creatures in the form of -1/-1 counters.)";

fn mana(kinds: &[(ManaType, usize)]) -> Vec<ManaUnit> {
    kinds
        .iter()
        .flat_map(|(kind, n)| {
            std::iter::repeat_with(move || ManaUnit::new(*kind, ObjectId(0), false, vec![]))
                .take(*n)
        })
        .collect()
}

fn counter_count(runner: &GameRunner, id: ObjectId, kind: &CounterType) -> u32 {
    runner
        .state()
        .objects
        .get(&id)
        .and_then(|obj| obj.counters.get(kind).copied())
        .unwrap_or(0)
}

fn life_of(runner: &GameRunner, player: PlayerId) -> i32 {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.life)
        .unwrap_or(-999)
}

fn add_angel(scenario: &mut GameScenario, controller: PlayerId) -> ObjectId {
    let mut builder = scenario.add_creature(controller, "Darksteel Angel", 4, 4);
    builder.from_oracle_text_with_keywords(&["Flying", "Indestructible"], DARKSTEEL_ANGEL_ORACLE);
    builder.id()
}

/// P1 attacks, P0 blocks, damage resolves: the shared combat driver for the
/// wither rows. Caller sets `active_player = P1` first.
fn run_combat_with_blocks(
    runner: &mut GameRunner,
    attackers: &[(ObjectId, AttackTarget)],
    blockers: &[(ObjectId, ObjectId)],
) -> Outcome {
    runner.advance_to_combat();
    runner
        .declare_attackers(attackers)
        .expect("declare attackers");
    for _ in 0..8 {
        if runner.waiting_for_kind() == "DeclareBlockers" {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass priority into the declare-blockers step");
    }
    runner.declare_blockers(blockers).expect("declare blockers");
    runner.combat_damage()
}

/// R0 — CR 614.17 + CR 120.3d: Solemnity ruling (2017-04-18): "Damage from a
/// source with wither has no effect on creatures. No -1/-1 counters are put
/// on them."
///
/// U2 isolation row: Solemnity parses at base, so this row reddens on the
/// wither bypass alone and greens on the routing fix — independent of the
/// Darksteel Angel parse (U1).
#[test]
fn solemnity_prevents_wither_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Solemnity", SOLEMNITY_ORACLE);
    let shieldmate = scenario.add_creature(P0, "Shieldmate", 2, 8).id();
    let butcher = scenario
        .add_creature_from_oracle(P1, "Smoldering Butcher", 4, 2, "Wither")
        .id();

    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    let outcome = run_combat_with_blocks(
        &mut runner,
        &[(butcher, AttackTarget::Player(P0))],
        &[(shieldmate, butcher)],
    );

    assert_eq!(
        counter_count(&runner, shieldmate, &CounterType::Minus1Minus1),
        0,
        "Solemnity must prevent the wither -1/-1 counters on the Shieldmate"
    );
    assert_eq!(
        outcome.zone_of(shieldmate),
        Zone::Battlefield,
        "the Shieldmate takes no counters and no damage, so it must survive"
    );
    assert_eq!(
        outcome.zone_of(butcher),
        Zone::Graveyard,
        "the Shieldmate's 2 damage is lethal to the 4/2 Butcher, proving combat damage resolved"
    );
}

/// R3 — CR 120.3d + CR 614.6 + CR 122.1: wither damage to a creature
/// Darksteel Angel's controller controls puts no -1/-1 counters on it;
/// wither damage to an opponent's creature still does (population scope).
///
/// The Shieldmate carries one pre-installed -1/-1 counter (direct-insert
/// fixture, per the Aboroth age-counter precedent): prevention adds none
/// and removes none, so the count stays exactly 1.
///
/// Revert probe: without the prohibition, the Shieldmate takes four more
/// counters from Smoldering Butcher (5 total), failing the counter
/// assertion.
#[test]
fn wither_damage_to_controlled_creature_puts_no_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let angel = add_angel(&mut scenario, P0);
    // 2/8: survives four -1/-1 counters, so the counter assertion (not just
    // a zone assertion) flips when the fix is reverted.
    let shieldmate = scenario.add_creature(P0, "Shieldmate", 2, 8).id();
    let gwyllion = scenario
        .add_creature_from_oracle(P0, "Harvest Gwyllion", 2, 4, "Wither")
        .id();
    let butcher = scenario
        .add_creature_from_oracle(P1, "Smoldering Butcher", 4, 2, "Wither")
        .id();
    let raider = scenario.add_creature(P1, "Raider", 3, 3).id();

    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&shieldmate)
        .expect("Shieldmate exists")
        .counters
        .insert(CounterType::Minus1Minus1, 1);
    runner.state_mut().active_player = P1;
    let outcome = run_combat_with_blocks(
        &mut runner,
        &[
            (butcher, AttackTarget::Player(P0)),
            (raider, AttackTarget::Player(P0)),
        ],
        &[(shieldmate, butcher), (gwyllion, raider)],
    );

    // The prohibited placement: four wither damage to the Shieldmate.
    assert_eq!(
        counter_count(&runner, shieldmate, &CounterType::Minus1Minus1),
        1,
        "Darksteel Angel must prevent new -1/-1 counters and remove none: exactly the pre-installed counter stays"
    );
    assert_eq!(
        outcome.zone_of(shieldmate),
        Zone::Battlefield,
        "the Shieldmate takes no new counters and no damage, so it must survive"
    );
    assert_eq!(
        runner.state().objects[&shieldmate].damage_marked,
        0,
        "CR 120.3d: wither damage is never marked, with or without the Angel"
    );
    // Population scope: the opponent's Raider is outside the prohibition.
    assert_eq!(
        counter_count(&runner, raider, &CounterType::Minus1Minus1),
        2,
        "Harvest Gwyllion's wither damage must still counter the opponent's Raider"
    );
    // Layers pin: the Raider's 3/3 body reads 1/1 through layer 7e.
    evaluate_layers(runner.state_mut());
    assert_eq!(
        runner.state().objects[&raider].power,
        Some(1),
        "the Raider's two -1/-1 counters must reduce its power to 1"
    );
    assert_eq!(
        runner.state().objects[&raider].toughness,
        Some(1),
        "the Raider's two -1/-1 counters must reduce its toughness to 1"
    );
    // The damage step ran: ordinary (non-wither) damage is still marked.
    // The Shieldmate's power is 2 minus its 1 pre-installed counter, so it
    // deals (and marks) exactly 1.
    assert_eq!(
        runner.state().objects[&butcher].damage_marked,
        1,
        "the Shieldmate's 1 damage must mark the Butcher, proving combat damage resolved"
    );
    assert_eq!(
        outcome.zone_of(angel),
        Zone::Battlefield,
        "the Angel is a bystander and must remain on the battlefield"
    );
    assert_eq!(
        outcome.zone_of(gwyllion),
        Zone::Battlefield,
        "the Gwyllion survives 3 marked damage on a 2/4 body"
    );
}

/// R3b — controller-relative binding: an opponent's Angel does not protect
/// your creatures. Pins `ControllerRef::You` to the replacement source's
/// controller (multi-authority hostile for the provenance contract).
/// Scope guard: green before and after the fix.
#[test]
fn opposing_angel_does_not_protect_your_creatures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let angel = add_angel(&mut scenario, P1);
    let shieldmate = scenario.add_creature(P0, "Shieldmate", 2, 8).id();
    let butcher = scenario
        .add_creature_from_oracle(P1, "Smoldering Butcher", 4, 2, "Wither")
        .id();

    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    let outcome = run_combat_with_blocks(
        &mut runner,
        &[(butcher, AttackTarget::Player(P0))],
        &[(shieldmate, butcher)],
    );

    assert_eq!(
        counter_count(&runner, shieldmate, &CounterType::Minus1Minus1),
        4,
        "P1's Angel protects P1's creatures only: the Shieldmate takes all four wither counters"
    );
    assert_eq!(
        outcome.zone_of(shieldmate),
        Zone::Battlefield,
        "the 2/8 Shieldmate survives four -1/-1 counters"
    );
    assert_eq!(
        outcome.zone_of(angel),
        Zone::Battlefield,
        "the Angel is a bystander and must remain on the battlefield"
    );
}

/// R4 — CR 122.1 + CR 614.17: +1/+1 counters are outside the prohibition's
/// counter-type scope, so Servant of the Scale still enters with its counter
/// under Darksteel Angel. Scope guard: green before and after the fix.
#[test]
fn plus_one_counters_still_land_on_controlled_creatures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, mana(&[(ManaType::Green, 1)]));

    let angel = add_angel(&mut scenario, P0);
    let servant = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Servant of the Scale",
            0,
            0,
            SERVANT_OF_THE_SCALE_ORACLE,
        )
        .with_mana_cost(parse_mtgjson_mana_cost("{G}"))
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(servant).resolve();

    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "casting Servant of the Scale must resolve to priority, got {:?}",
        outcome.final_waiting_for()
    );
    assert_eq!(
        outcome.zone_of(servant),
        Zone::Battlefield,
        "Servant of the Scale must enter the battlefield"
    );
    assert_eq!(
        counter_count(&runner, servant, &CounterType::Plus1Plus1),
        1,
        "Servant of the Scale must still enter with its +1/+1 counter under the Angel"
    );
    assert_eq!(
        outcome.zone_of(angel),
        Zone::Battlefield,
        "the Angel is a bystander and must remain on the battlefield"
    );
}

/// R5 — CR 614.17 + entry replacement handling: Kitchen Finks' persist
/// re-entry under the Angel gets no -1/-1 counter (Blightbeetle ruling:
/// entry counters are prevented). Life 24 = 20 + 2 (cast entry) + 2
/// (persist re-entry) proves the Finks re-entered rather than survived.
#[test]
fn persist_reentry_under_angel_gets_no_counter() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        mana(&[
            (ManaType::Colorless, 1),
            (ManaType::Green, 1),
            (ManaType::White, 1),
        ]),
    );

    let angel = add_angel(&mut scenario, P0);
    let finks = scenario
        .add_creature_to_hand_from_oracle(P0, "Kitchen Finks", 3, 2, KITCHEN_FINKS_ORACLE)
        .with_mana_cost(parse_mtgjson_mana_cost("{1}{G/W}{G/W}"))
        .id();
    let attacker = scenario.add_creature(P1, "Attacker", 3, 3).id();

    let mut runner = scenario.build();
    let cast_outcome = runner.cast(finks).resolve();
    assert!(
        matches!(
            cast_outcome.final_waiting_for(),
            WaitingFor::Priority { .. }
        ),
        "casting Kitchen Finks must resolve to priority, got {:?}",
        cast_outcome.final_waiting_for()
    );
    assert_eq!(life_of(&runner, P0), 22, "the cast entry must gain 2 life");

    // The 3/2 Finks blocks the 3/3 attacker and dies; persist returns it.
    runner.state_mut().active_player = P1;
    run_combat_with_blocks(
        &mut runner,
        &[(attacker, AttackTarget::Player(P0))],
        &[(finks, attacker)],
    );
    runner.advance_until_stack_empty();

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the persist trigger must resolve to priority, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().stack.is_empty(),
        "the stack must be empty after persist resolves"
    );
    assert_eq!(
        runner.state().objects[&finks].zone,
        Zone::Battlefield,
        "persist must return the Finks to the battlefield"
    );
    assert_eq!(
        counter_count(&runner, finks, &CounterType::Minus1Minus1),
        0,
        "the Angel must prevent persist's -1/-1 counter on re-entry"
    );
    assert_eq!(
        life_of(&runner, P0),
        24,
        "both entries must trigger the life gain (cast + persist re-entry)"
    );
    assert_eq!(
        runner.state().objects[&angel].zone,
        Zone::Battlefield,
        "the Angel is a bystander and must remain on the battlefield"
    );
}

/// R6 — CR 120.3d: wither counters are put-counters events, so Winding
/// Constrictor's "that many plus one … instead" replacement sees them.
/// U2 isolation row (no Angel involved): reddens on the bypass alone.
#[test]
fn constrictor_sees_wither_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let constrictor = scenario
        .add_creature_from_oracle(P0, "Winding Constrictor", 2, 3, WINDING_CONSTRICTOR_ORACLE)
        .id();
    let victim = scenario.add_creature(P0, "Victim", 2, 8).id();
    let ripper = scenario
        .add_creature_from_oracle(P1, "Sickle Ripper", 2, 1, "Wither")
        .id();

    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    let outcome = run_combat_with_blocks(
        &mut runner,
        &[(ripper, AttackTarget::Player(P0))],
        &[(victim, ripper)],
    );

    assert_eq!(
        counter_count(&runner, victim, &CounterType::Minus1Minus1),
        3,
        "the Constrictor must add one to the Ripper's two wither counters"
    );
    assert_eq!(
        outcome.zone_of(victim),
        Zone::Battlefield,
        "the 2/8 victim survives three -1/-1 counters"
    );
    assert_eq!(
        outcome.zone_of(constrictor),
        Zone::Battlefield,
        "the Constrictor is a bystander and must remain on the battlefield"
    );
}

/// R7 — CR 120.3d + CR 702.15b: wither damage is still damage dealt, so
/// lifelink gains life on it (Blossombind ruling). Child of Night ({1}{B},
/// set for black color) gains wither from Corrosive Mentor and keeps its
/// native lifelink. Preservation row: green before and after the fix.
#[test]
fn lifelink_still_sees_wither_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let mentor = scenario
        .add_creature_from_oracle(P0, "Corrosive Mentor", 1, 3, CORROSIVE_MENTOR_ORACLE)
        .id();
    let child = scenario
        .add_creature_from_oracle(P0, "Child of Night", 2, 1, "Lifelink")
        .with_mana_cost(parse_mtgjson_mana_cost("{1}{B}"))
        .id();
    let mite = scenario.add_creature(P1, "Mite", 1, 4).id();

    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    let outcome = run_combat_with_blocks(
        &mut runner,
        &[(mite, AttackTarget::Player(P0))],
        &[(child, mite)],
    );

    assert_eq!(
        life_of(&runner, P0),
        22,
        "lifelink must gain 2 life on the Child's wither damage"
    );
    assert_eq!(
        counter_count(&runner, mite, &CounterType::Minus1Minus1),
        2,
        "the Mentor-granted wither must counter the Mite"
    );
    assert_eq!(
        outcome.zone_of(child),
        Zone::Graveyard,
        "simultaneous damage: the Child deals 2 and takes lethal 1"
    );
    assert_eq!(
        outcome.zone_of(mentor),
        Zone::Battlefield,
        "the Mentor is a bystander and must remain on the battlefield"
    );
}

/// R8 — CR 614.17b + CR 702.24a mirror of
/// `aboroth_with_an_age_counter_under_solemnity_cannot_choose_to_pay`:
/// Aboroth's cumulative upkeep puts a -1/-1 counter on itself, which the
/// Angel's prohibition refuses at cost-preview time, so CR 702.24a's
/// "sacrifice it" happens instead.
#[test]
fn aboroth_upkeep_unpayable_under_darksteel_angel() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    add_angel(&mut scenario, P0);
    let aboroth = scenario
        .add_creature_from_oracle(P0, "Aboroth", 9, 9, ABOROTH_ORACLE)
        .id();
    // Control: an ordinary creature with no cumulative upkeep. The mechanism
    // under test cannot sacrifice it, so it must NOT reach the graveyard.
    let control = scenario.add_creature(P0, "Control Bear", 2, 2).id();
    // Without a library the suppressed prompt lets the turn run into the Draw
    // step, P0 decks out, and EVERY P0 object is exiled — which satisfies a
    // `!= Battlefield` assertion for the control creature too.
    scenario.with_library_top(P0, &["Filler A", "Filler B", "Filler C"]);
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&aboroth)
        .expect("Aboroth exists")
        .counters
        .insert(CounterType::Age, 1);

    // Aboroth's upkeep cost only materialises at >= 1 age counter.
    runner.auto_advance_to_main_phase();
    runner.advance_until_stack_empty();

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "the prohibited pay branch must leave no unless-payment prompt, got {:?}",
        runner.state().waiting_for
    );
    let legal = engine::ai_support::legal_actions(runner.state());
    assert!(
        !legal.is_empty(),
        "the action vector must still be live, got {legal:?}"
    );
    assert!(
        !legal.contains(&GameAction::PayUnlessCost { pay: true }),
        "paying a -1/-1 counter cost must not be legal under the Angel, got {legal:?}"
    );

    // CR 702.24a: an unpaid cumulative upkeep sacrifices the permanent.
    assert_eq!(
        runner
            .state()
            .objects
            .get(&aboroth)
            .map(|obj| obj.zone)
            .expect("Aboroth object still exists"),
        Zone::Graveyard,
        "an unpayable cumulative upkeep must sacrifice Aboroth"
    );
    assert_eq!(
        runner
            .state()
            .objects
            .get(&control)
            .map(|obj| obj.zone)
            .expect("the control creature still exists"),
        Zone::Battlefield,
        "the control creature has no cumulative upkeep and must not be sacrificed"
    );
}
