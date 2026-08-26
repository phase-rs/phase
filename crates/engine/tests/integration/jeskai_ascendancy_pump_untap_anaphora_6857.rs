//! Issue #6857 — event-less producers publish the population they froze.
//!
//! `Effect::PumpAll`, `Effect::GoadAll` and `Effect::GiveControl` affect objects
//! without moving them and without emitting any per-object event, so before this
//! change the chain publish site fell through to the `ZoneChanged` harvest and
//! published an EMPTY tracked set. CR 611.2c makes that the WRONG set, not just
//! an unhelpful one: the set of objects a resolution-generated continuous effect
//! modifies is fixed when the effect begins. A following "Untap those creatures"
//! (CR 701.26b) therefore bound nothing — Jeskai Ascendancy's loot-and-untap did
//! not untap.
//!
//! Every row here is measured on the shipped tree. The suite carries its own
//! anti-vacuity instruments:
//!
//!   * `known_changed_control_*` — the row that MUST differ from the old
//!     behaviour. If it ever passes trivially the whole file is meaningless.
//!   * `negative_control_*` — a chain with no consumer at all: the arms must
//!     invent no publish.
//!   * `leg1_witness_*` / `leg2_witness_*` — each pins one leg of
//!     `is_sole_chain_producer`. Deleting that leg from the engine must turn the
//!     named test RED; a leg whose deletion changes nothing is vacuous.
//!   * the PRESERVED rows assert the publish did NOT widen a filter or reach a
//!     grant that declares its own target.
//!
//! Oracle text is verbatim at the branch base unless a deviation is called out
//! in the test's doc comment.

use engine::game::combat::AttackTarget;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{BattlefieldEntryRecord, CastPaymentMode, GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;

/// Every tracked set, id-ordered, each as a sorted list of raw object ids.
fn tracked_sets(state: &GameState) -> Vec<Vec<u64>> {
    let mut sets: Vec<(u64, Vec<u64>)> = state
        .tracked_object_sets
        .iter()
        .map(|(id, members)| {
            let mut ids: Vec<u64> = members.iter().map(|o| o.0).collect();
            ids.sort_unstable();
            (id.0, ids)
        })
        .collect();
    sets.sort();
    sets.into_iter().map(|(_, ids)| ids).collect()
}

/// The single chain tracked set's contents. Panics if the resolution published
/// more than one set — every row in this file is a one-producer chain, and a
/// second set would mean the guard let two producers through.
fn published_set(state: &GameState) -> Vec<u64> {
    let sets = tracked_sets(state);
    assert!(
        sets.len() <= 1,
        "expected at most one tracked set in a single-producer chain, got {sets:?}"
    );
    sets.into_iter().next().unwrap_or_default()
}

fn ids(objects: &[ObjectId]) -> Vec<u64> {
    let mut raw: Vec<u64> = objects.iter().map(|o| o.0).collect();
    raw.sort_unstable();
    raw
}

fn tapped(state: &GameState, id: ObjectId) -> bool {
    state.objects[&id].tapped
}

/// Debug rendering of every transient continuous effect that applies to `id`.
/// The continuous-effect list is the observable for the `GenericEffect` grants
/// (MustAttack / CantBlock / keyword grants) a mass head feeds; a
/// tracked-set-only projection previously scored a non-fix as a fix.
fn effects_on(state: &GameState, id: ObjectId) -> Vec<String> {
    state
        .transient_continuous_effects
        .iter()
        .filter(|tce| tce.affected == engine::types::ability::TargetFilter::SpecificObject { id })
        .map(|tce| format!("{:?}", tce.modifications))
        .collect()
}

fn grant_lands_on(state: &GameState, id: ObjectId, needle: &str) -> bool {
    effects_on(state, id).iter().any(|m| m.contains(needle))
}

// ===========================================================================
// CONTROLS
// ===========================================================================

/// KNOWN-CHANGED CONTROL — issue #6857's own card, cast as the printed card.
/// Jeskai Ascendancy's first trigger is `PumpAll -> SetTapState
/// { target: TrackedSet, Untap }`. Before the fix the published set was empty
/// and the creature stayed TAPPED; it must now be published and untapped.
///
/// The real enchantment is used deliberately rather than a synthesized trigger
/// body: this is the control for #6857, so it should exercise #6857's card, on
/// its real trigger path, with the second (loot) trigger present. If this row
/// ever reads the same as the pre-fix engine, every "identical" reading
/// elsewhere in this file is meaningless.
#[test]
fn known_changed_control_jeskai_ascendancy_untaps_the_creatures_it_pumped() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mine = scenario.add_creature(P0, "Mine", 3, 3).id();
    scenario.add_enchantment_from_oracle(
        P0,
        "Jeskai Ascendancy",
        "Whenever you cast a noncreature spell, creatures you control get +1/+1 until end of turn. Untap those creatures.\nWhenever you cast a noncreature spell, you may draw a card. If you do, discard a card.",
    );
    let bolt = scenario.add_bolt_to_hand(P0);
    let mut runner: GameRunner = scenario.build();
    runner.state_mut().objects.get_mut(&mine).unwrap().tapped = true;
    // Both of the printed card's triggers fire on the same cast, so the engine
    // parks an APNAP ordering prompt (CR 603.3b) that the one-shot cast driver
    // does not handle. Drive the cast by hand rather than trimming the card to
    // dodge the prompt: the point of this control is that it uses #6857's card.
    let card_id = runner.state().objects[&bolt].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: bolt,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting the bolt should be legal");
    // Drain the prompts the printed card creates: the bolt's own target, the
    // CR 603.3b ordering prompt for the two simultaneous cast triggers, and the
    // second trigger's "you may draw a card" (declined — this row is about the
    // first trigger).
    for _ in 0..32 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Player(P1)),
                    })
                    .expect("the bolt targets a player");
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .expect("CR 603.3b: order the two cast triggers");
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: false })
                    .expect("CR 608.2d: decline the loot trigger");
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority resolves the top of the stack");
            }
            _ => break,
        }
    }
    runner.advance_until_stack_empty();
    evaluate_layers(runner.state_mut());

    assert_eq!(published_set(runner.state()), ids(&[mine]));
    assert!(
        !tapped(runner.state(), mine),
        "CR 701.26b: 'those creatures' names the pumped population, so it untaps"
    );
    assert_eq!(runner.state().objects[&mine].power, Some(4), "pump applied");
}

/// NEGATIVE CONTROL — a mass pump with no anaphor at all. The publish gate never
/// fires, so the new arms must invent no set.
#[test]
fn negative_control_mass_pump_without_a_consumer_publishes_nothing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mine = scenario.add_creature(P0, "Mine", 3, 3).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Bare Pump",
            true,
            "Creatures you control get +1/+1 until end of turn.",
        )
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner: GameRunner = scenario.build();
    runner.cast(spell).resolve();
    evaluate_layers(runner.state_mut());

    assert!(tracked_sets(runner.state()).is_empty());
    assert!(runner.state().chain_tracked_set_id.is_none());
    assert_eq!(runner.state().objects[&mine].power, Some(4), "pump applied");
}

// ===========================================================================
// `PumpAll` — FIX rows
// ===========================================================================

/// War Flare's second sentence pair — the plainest `PumpAll -> SetTapState
/// { TrackedSet }` shape in the corpus.
#[test]
fn war_flare_untaps_the_creatures_it_pumped() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let a = scenario.add_creature(P0, "Mine A", 2, 2).id();
    let b = scenario.add_creature(P0, "Mine B", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "War Flare",
            true,
            "Creatures you control get +2/+1 until end of turn. Untap those creatures.",
        )
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner: GameRunner = scenario.build();
    for id in [a, b] {
        runner.state_mut().objects.get_mut(&id).unwrap().tapped = true;
    }
    runner.cast(spell).resolve();
    evaluate_layers(runner.state_mut());

    assert_eq!(published_set(runner.state()), ids(&[a, b]));
    assert!(!tapped(runner.state(), a) && !tapped(runner.state(), b));
}

/// Gleam of Resistance — the REAL card, including its basic landcycling line, so
/// the fixture cannot be a simplified proxy of the shape under test (the
/// `Typecycling` keyword on the built object is the discriminator).
///
/// The opponent's creature staying tapped is the load-bearing half: it proves
/// the published population kept the head filter's `controller: You` rather than
/// being widened to the whole battlefield.
#[test]
fn gleam_of_resistance_untaps_only_the_creatures_its_controller_filter_named() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let a = scenario.add_creature(P0, "Mine A", 2, 2).id();
    let b = scenario.add_creature(P0, "Mine B", 2, 2).id();
    let theirs = scenario.add_creature(P1, "Theirs", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Gleam of Resistance",
            true,
            "Creatures you control get +1/+2 until end of turn. Untap those creatures.\nBasic landcycling {1}{W} ({1}{W}, Discard this card: Search your library for a basic land card, reveal it, put it into your hand, then shuffle.)",
        )
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner: GameRunner = scenario.build();
    for id in [a, b, theirs] {
        runner.state_mut().objects.get_mut(&id).unwrap().tapped = true;
    }
    assert!(
        format!("{:?}", runner.state().objects[&spell].keywords).contains("Typecycling"),
        "fixture guard: the full printed card was built, not a pump-only proxy"
    );
    runner.cast(spell).resolve();
    evaluate_layers(runner.state_mut());

    assert_eq!(published_set(runner.state()), ids(&[a, b]));
    assert!(!tapped(runner.state(), a) && !tapped(runner.state(), b));
    assert!(
        tapped(runner.state(), theirs),
        "CR 611.2c: the frozen population is the head filter's, and it says 'you control'"
    );
}

/// Zealous Display's untap carries `condition: Not(IsYourTurn)`, so it is cast on
/// the OPPONENT's turn. Cast on your own turn the sub never executes and the row
/// is vacuously identical to the pre-fix engine.
#[test]
fn zealous_display_untaps_on_the_opponents_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let a = scenario.add_creature(P0, "Mine A", 2, 2).id();
    let b = scenario.add_creature(P0, "Mine B", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Zealous Display",
            true,
            "Creatures you control get +2/+0 until end of turn. If it's not your turn, untap those creatures.",
        )
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner: GameRunner = scenario.build();
    // Fixture setup: hand the turn to the opponent so `Not(IsYourTurn)` holds.
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = engine::types::game_state::WaitingFor::Priority { player: P0 };
    for id in [a, b] {
        runner.state_mut().objects.get_mut(&id).unwrap().tapped = true;
    }
    assert_ne!(
        runner.state().active_player,
        P0,
        "fixture guard: on your own turn the untap sub never runs and this row is vacuous"
    );
    runner.cast(spell).resolve();
    evaluate_layers(runner.state_mut());

    assert_eq!(published_set(runner.state()), ids(&[a, b]));
    assert!(!tapped(runner.state(), a) && !tapped(runner.state(), b));
}

/// Motivated Pony's attack trigger. Its untap is gated on
/// `BattlefieldEntriesThisTurn { Food } >= 1`, so a Food entry is stamped into
/// the ledger — without it the branch never executes and the row is vacuous.
/// Only ATTACKING creatures may enter the published set, which is what
/// keeps the `Attacking` property in the head filter honest.
#[test]
fn motivated_pony_untaps_only_the_attacking_creatures_it_pumped() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let pony = scenario
        .add_creature_from_oracle(
            P0,
            "Motivated Pony",
            3,
            3,
            "Trample, haste\nWhenever this creature attacks, attacking creatures get +1/+1 until end of turn. If a Food entered the battlefield under your control this turn, untap those creatures and they get an additional +2/+2 until end of turn.",
        )
        .id();
    let buddy = scenario.add_creature(P0, "Buddy", 2, 2).id();
    let home = scenario.add_creature(P0, "Stays Home", 2, 2).id();
    let mut runner: GameRunner = scenario.build();
    runner.state_mut().objects.get_mut(&buddy).unwrap().keywords =
        vec![engine::types::keywords::Keyword::Haste];
    // Fixture setup: a Food entered the battlefield this turn, so the
    // intervening-if holds and the untap branch actually runs.
    runner
        .state_mut()
        .battlefield_entries_this_turn
        .push(BattlefieldEntryRecord {
            object_id: ObjectId(9_999),
            name: "Food".to_string(),
            core_types: vec![CoreType::Artifact],
            subtypes: vec!["Food".to_string()],
            supertypes: vec![],
            colors: vec![],
            keywords: vec![],
            controller: P0,
        });
    runner.advance_to_combat();
    runner
        .declare_attackers(&[
            (pony, AttackTarget::Player(P1)),
            (buddy, AttackTarget::Player(P1)),
        ])
        .expect("fixture guard: the attack trigger must actually fire");
    runner.advance_until_stack_empty();
    evaluate_layers(runner.state_mut());

    assert_eq!(published_set(runner.state()), ids(&[pony, buddy]));
    assert!(!tapped(runner.state(), pony) && !tapped(runner.state(), buddy));
    assert!(
        !published_set(runner.state()).contains(&home.0),
        "CR 611.2c: the non-attacker was never in the frozen population"
    );
}

/// Suicidal Charge — the mass head feeds a `GenericEffect { affected:
/// ParentTarget, MustAttack }` coercion instead of an untap. Before the fix the
/// opponent's creatures were shrunk but not coerced: half the card did nothing.
#[test]
fn suicidal_charge_coerces_the_creatures_it_shrank() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let a = scenario.add_creature(P1, "Theirs A", 3, 3).id();
    let b = scenario.add_creature(P1, "Theirs B", 2, 2).id();
    let src = scenario
        .add_enchantment_from_oracle(
            P0,
            "Suicidal Charge",
            "Sacrifice this enchantment: Creatures your opponents control get -1/-1 until end of turn. Those creatures attack this turn if able.",
        )
        .id();
    let mut runner: GameRunner = scenario.build();
    runner.activate(src, 0).resolve();
    evaluate_layers(runner.state_mut());

    assert_eq!(published_set(runner.state()), ids(&[a, b]));
    for id in [a, b] {
        assert!(
            grant_lands_on(runner.state(), id, "MustAttack"),
            "CR 608.2c: 'those creatures' names the shrunk population"
        );
    }
    assert_eq!(runner.state().objects[&a].power, Some(2), "shrink applied");
}

// ===========================================================================
// `PumpAll` — PRESERVED rows
// ===========================================================================

/// Elvish Elegy: `Mill -> PumpAll -> ChangeZoneAll { TrackedSetFiltered }`.
///
/// LEG-1 ROW. The `Mill` already published the milled cards, so the mass pump is
/// not the antecedent of "from among the milled cards" and must not join the
/// set. (`leg1_witness_surge_to_victory_*` is the sharper revert probe for the
/// same leg; this row covers the same leg on a `PumpAll` whose own enumeration
/// happens to be empty.)
#[test]
fn elvish_elegy_keeps_the_milled_set_free_of_the_graveyard_pump() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Lib Elf", "Lib Land", "Lib Bear"]);
    scenario.with_graveyard(P0, &["Yard Creature"]);
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Elvish Elegy",
            false,
            "Mill three cards, then each creature card in your graveyard perpetually gets +1/+1. You may put an Elf or land card from among the milled cards into your hand.",
        )
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner: GameRunner = scenario.build();
    runner.cast(spell).resolve();

    let set = published_set(runner.state());
    assert_eq!(
        set.len(),
        3,
        "the milled cards, and only those: got {set:?}"
    );
}

/// Heroic Charge, cast UNKICKED. Its trample grant sits behind the kicked
/// condition, so publishing the pumped population must not make it execute.
#[test]
fn heroic_charge_unkicked_publishes_without_granting_trample() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let a = scenario.add_creature(P0, "Mine A", 3, 3).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Heroic Charge",
            false,
            "Kicker {1}{R} (You may pay an additional {1}{R} as you cast this spell.)\nCreatures you control get +2/+1 until end of turn. If this spell was kicked, those creatures also gain trample until end of turn.",
        )
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner: GameRunner = scenario.build();
    runner.cast(spell).resolve();
    evaluate_layers(runner.state_mut());

    assert_eq!(
        runner.state().objects[&a].power,
        Some(5),
        "non-vacuity: the pump ran, so the chain really resolved"
    );
    assert!(
        !grant_lands_on(runner.state(), a, "Trample"),
        "the kicked-only grant must not fire on an unkicked cast"
    );
}

/// Valley Rally with its condition removed, so the targeted grant actually
/// executes. The head is a population and the grant DECLARES its own target: the
/// grant node's own targets must win over the published set.
///
/// DISCLOSED DEVIATION: the printed card gates the grant on `AdditionalCostPaid`
/// (the gift). That branch never runs in this harness, which would make the row
/// vacuous, so the condition is dropped and everything else kept.
#[test]
fn valley_rally_grant_binds_its_own_target_not_the_published_population() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let a = scenario.add_creature(P0, "Mine A", 3, 3).id();
    let b = scenario.add_creature(P0, "Mine B", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Valley Rally Grant Path",
            true,
            "Creatures you control get +2/+0 until end of turn. Target creature you control gains first strike until end of turn.",
        )
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner: GameRunner = scenario.build();
    runner.cast(spell).target_objects(&[a]).resolve();
    evaluate_layers(runner.state_mut());

    assert_eq!(
        runner.state().objects[&b].power,
        Some(4),
        "non-vacuity: the mass pump reached the non-targeted creature"
    );
    assert!(grant_lands_on(runner.state(), a, "FirstStrike"));
    assert!(
        !grant_lands_on(runner.state(), b, "FirstStrike"),
        "CR 608.2c: a grant with its own declared target does not read the frozen population"
    );
}

// ===========================================================================
// `GoadAll`
// ===========================================================================

/// Kaima, the Fractured Calm — the consumer is a COUNT
/// (`FilteredTrackedSetSize`), not an anaphor, so the observable is Kaima's
/// counter total. Only the ENCHANTED opponent creature may be counted, which is
/// what proves the head filter's `HasAttachment { Aura }` property survived into
/// the published population.
///
/// DISCLOSED DEVIATION: given as an activated ability so `SelfRef` denotes the
/// permanent and the chain runs without waiting for the printed trigger.
#[test]
fn kaima_counts_only_the_enchanted_creature_it_goaded() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let victim = scenario.add_creature(P1, "Enchanted Victim", 3, 3).id();
    let plain = scenario.add_creature(P1, "Plain Victim", 2, 2).id();
    let aura = scenario
        .add_enchantment_from_oracle(P0, "Kaima Aura", "Enchant creature")
        .id();
    let kaima = scenario
        .add_creature_from_oracle(
            P0,
            "Kaima Body",
            3,
            3,
            "{T}: Goad each creature your opponents control that's enchanted by an Aura you control. Put a +1/+1 counter on Kaima Body for each creature goaded this way.",
        )
        .id();
    let mut runner: GameRunner = scenario.build();
    runner.attach_as_bestowed_aura(aura, victim);
    runner.activate(kaima, 0).resolve();
    evaluate_layers(runner.state_mut());

    assert_eq!(published_set(runner.state()), ids(&[victim]));
    assert!(
        !published_set(runner.state()).contains(&plain.0),
        "CR 611.2c: the unenchanted creature was never in the frozen population"
    );
    assert_eq!(
        runner.state().objects[&kaima]
            .counters
            .get(&engine::types::counter::CounterType::Plus1Plus1)
            .copied(),
        Some(1),
        "one creature goaded this way"
    );
}

/// Taunt from the Rampart — `GoadAll` feeding a `GenericEffect { affected:
/// ParentTarget, CantBlock }`.
#[test]
fn taunt_from_the_rampart_stops_the_creatures_it_goaded_from_blocking() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let theirs = scenario.add_creature(P1, "Theirs A", 3, 3).id();
    let mine = scenario.add_creature(P0, "Mine", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Taunt from the Rampart",
            true,
            "Goad all creatures your opponents control. Until your next turn, those creatures can't block. (Until your next turn, those creatures attack each combat if able and attack a player other than you if able.)",
        )
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner: GameRunner = scenario.build();
    runner.cast(spell).resolve();
    evaluate_layers(runner.state_mut());

    assert_eq!(published_set(runner.state()), ids(&[theirs]));
    assert!(grant_lands_on(runner.state(), theirs, "CantBlock"));
    assert!(
        !grant_lands_on(runner.state(), mine, "CantBlock"),
        "CR 701.15a: only the goaded creatures are named"
    );
}

// ===========================================================================
// `GiveControl`
// ===========================================================================

/// Domineering Will — the authority test for `GiveControl`. "Those creatures"
/// names the DECLARED TARGETS (CR 608.2c), and a target the recipient already
/// controls emits no `ControllerChanged`, so an event-harvest authority would
/// leave it tapped. Here the recipient is P0 and one target is already P0's, so
/// the two candidate authorities disagree and the event one fails.
#[test]
fn domineering_will_untaps_a_target_the_recipient_already_controlled() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let theirs = scenario.add_creature(P1, "Theirs", 2, 2).id();
    let already_mine = scenario.add_creature(P0, "Already Mine", 1, 1).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Domineering Will",
            true,
            "Target player gains control of up to three target nonattacking creatures until end of turn. Untap those creatures. They block this turn if able.",
        )
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner: GameRunner = scenario.build();
    for id in [theirs, already_mine] {
        runner.state_mut().objects.get_mut(&id).unwrap().tapped = true;
    }
    runner
        .cast(spell)
        .target_player(P0)
        .target_objects(&[theirs, already_mine])
        .resolve();
    evaluate_layers(runner.state_mut());

    assert_eq!(published_set(runner.state()), ids(&[theirs, already_mine]));
    assert!(!tapped(runner.state(), theirs));
    assert!(
        !tapped(runner.state(), already_mine),
        "CR 608.2c: a declared target that changed no controller is still one of 'those creatures'"
    );
}

/// Coveted Falcon's turn-face-up trigger body: `GiveControl -> Draw
/// { TrackedSetSize }`. The observable is cards drawn, which was 0 before the
/// fix.
#[test]
fn coveted_falcon_draws_for_each_permanent_handed_over() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let a = scenario.add_creature(P0, "Give A", 1, 1).id();
    scenario.add_card_to_library_top(P0, "Library Card A");
    scenario.add_card_to_library_top(P0, "Library Card B");
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Falcon Trigger Body",
            true,
            "Target opponent gains control of any number of target permanents you control. Draw a card for each one they gained control of this way.",
        )
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner: GameRunner = scenario.build();
    let before = runner.state().players[0].hand.len();
    let outcome = runner.cast(spell).target_objects(&[a]).resolve();

    assert_eq!(published_set(outcome.state()), ids(&[a]));
    assert_eq!(
        outcome.state().players[0].hand.len(),
        before,
        "one card drawn, and the spell itself left the hand"
    );
    assert_eq!(
        outcome.state().objects[&a].controller,
        P1,
        "non-vacuity: control actually changed"
    );
}

// ===========================================================================
// LEG WITNESSES — each pins one leg of `is_sole_chain_producer`
// ===========================================================================

/// LEG-1 WITNESS (the sharper of the two: it flips a set's CONTENTS, not just a
/// boolean). Surge to Victory exiles a card and then mass-pumps; "the exiled
/// card" names the exile, not the creatures. The `ChangeZone` ancestor already
/// published, so the mass pump must decline.
///
/// Deleting `no_earlier_producer` makes the pumped creature join the set.
#[test]
fn leg1_witness_surge_to_victory_binds_the_exiled_card_not_the_pumped_creatures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature(P0, "Alpha", 2, 2);
    let graveyard_card = scenario.add_spell_to_graveyard(P0, "Shock", true).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Surge to Victory",
            false,
            "Exile target instant or sorcery card from your graveyard. Creatures you control get +X/+0 until end of turn, where X is that card's mana value. Whenever a creature you control deals combat damage to a player this turn, copy the exiled card. You may cast the copy without paying its mana cost.",
        )
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner: GameRunner = scenario.build();
    runner
        .cast(spell)
        .target_objects(&[graveyard_card])
        .resolve();
    evaluate_layers(runner.state_mut());

    assert_eq!(
        published_set(runner.state()),
        ids(&[graveyard_card]),
        "CR 608.2c: the anaphor names the exile, so the pumped creature must stay out"
    );
    assert_eq!(
        runner.state().objects[&graveyard_card].zone,
        engine::types::zones::Zone::Exile,
        "non-vacuity: the exile really happened"
    );
}

/// LEG-2 WITNESS. Outlaws' Fury pumps FIRST and exiles afterwards, so the later
/// exile is the antecedent of "you may play that card" and the mass pump must
/// decline even though nothing published before it.
///
/// Deleting `!later_node_is_publisher_position` makes the pumped creatures join
/// the exiled card's set, and the play permission would then cover creatures.
#[test]
fn leg2_witness_outlaws_fury_binds_the_later_exile_not_the_pumped_creatures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let alpha = scenario.add_creature(P0, "Alpha", 2, 2).id();
    scenario
        .add_creature(P0, "Rogue Pal", 1, 1)
        .with_subtypes(vec!["Rogue"]);
    scenario.with_library_top(P0, &["Lib A", "Lib B"]);
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Outlaws' Fury",
            false,
            "Creatures you control get +2/+0 until end of turn. If you control an outlaw, exile the top card of your library. Until the end of your next turn, you may play that card. (Assassins, Mercenaries, Pirates, Rogues, and Warlocks are outlaws.)",
        )
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner: GameRunner = scenario.build();
    runner.cast(spell).resolve();
    evaluate_layers(runner.state_mut());

    let set = published_set(runner.state());
    assert_eq!(set.len(), 1, "exactly the exiled card: got {set:?}");
    assert!(
        !set.contains(&alpha.0),
        "CR 608.2c: a head followed by another producer is not the antecedent"
    );
    assert_eq!(
        runner.state().objects[&alpha].power,
        Some(4),
        "non-vacuity: the mass pump ran, it simply did not publish"
    );
}

// ===========================================================================
// PARSER HALF — the implicit-pronoun anaphor ("Untap them.")
// ===========================================================================

/// PARSER KNOWN-CHANGED CONTROL — Rallying Roar. Verbatim. Its untap is an
/// implicit pronoun, which the spell-body default lowers to `ParentTarget`; only
/// the parser rewrite turns it into `TrackedSet(0)`. If this passes without the
/// rewrite, every other parser row here is meaningless.
#[test]
fn parser_control_rallying_roar_untaps_the_creatures_it_pumped() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let a = scenario.add_creature(P0, "Mine A", 2, 2).id();
    let b = scenario.add_creature(P0, "Mine B", 2, 2).id();
    let theirs = scenario.add_creature(P1, "Theirs", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Rallying Roar",
            true,
            "Creatures you control get +1/+1 until end of turn. Untap them.",
        )
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner: GameRunner = scenario.build();
    for id in [a, b, theirs] {
        runner.state_mut().objects.get_mut(&id).unwrap().tapped = true;
    }
    runner.cast(spell).resolve();
    evaluate_layers(runner.state_mut());

    assert_eq!(published_set(runner.state()), ids(&[a, b]));
    assert!(!tapped(runner.state(), a) && !tapped(runner.state(), b));
    assert!(tapped(runner.state(), theirs), "controller filter survives");
}

/// Rally to Battle — same shape, different numbers; kept as its own row because
/// the roster is per-card.
#[test]
fn rally_to_battle_untaps_the_creatures_it_pumped() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let a = scenario.add_creature(P0, "Mine A", 2, 2).id();
    let b = scenario.add_creature(P0, "Mine B", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Rally to Battle",
            true,
            "Creatures you control get +1/+3 until end of turn. Untap them.",
        )
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner: GameRunner = scenario.build();
    for id in [a, b] {
        runner.state_mut().objects.get_mut(&id).unwrap().tapped = true;
    }
    runner.cast(spell).resolve();
    evaluate_layers(runner.state_mut());

    assert_eq!(published_set(runner.state()), ids(&[a, b]));
    assert!(!tapped(runner.state(), a) && !tapped(runner.state(), b));
    assert_eq!(runner.state().objects[&a].toughness, Some(5));
}

/// Great Oak Guardian's ETB trigger — the population is `target player`'s
/// creatures, so targeting the OPPONENT makes the anaphor's scope observable:
/// their creatures untap and mine do not. A rewrite that bound "them" to the
/// source or to the parent target could not produce this split.
#[test]
fn great_oak_guardian_untaps_the_targeted_players_creatures_only() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let theirs = scenario.add_creature(P1, "Theirs", 2, 2).id();
    let mine = scenario.add_creature(P0, "Mine", 2, 2).id();
    let spell = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Great Oak Guardian",
            4,
            5,
            "Flash (You may cast this spell any time you could cast an instant.)\nReach\nWhen this creature enters, creatures target player controls get +2/+2 until end of turn. Untap them.",
        )
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner: GameRunner = scenario.build();
    for id in [theirs, mine] {
        runner.state_mut().objects.get_mut(&id).unwrap().tapped = true;
    }
    runner.cast(spell).target_player(P1).resolve();
    runner.advance_until_stack_empty();
    evaluate_layers(runner.state_mut());

    assert_eq!(published_set(runner.state()), ids(&[theirs]));
    assert!(!tapped(runner.state(), theirs));
    assert_eq!(runner.state().objects[&theirs].power, Some(4), "pumped");
    assert!(
        tapped(runner.state(), mine),
        "CR 611.2c: the frozen population is the TARGETED player's creatures"
    );
}

/// The General — the same anaphor under an activated ability with a
/// self-exile cost, i.e. the population head is not the ability source.
#[test]
fn the_general_untaps_the_creatures_it_pumped() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let a = scenario.add_creature(P0, "Mine A", 2, 2).id();
    let src = scenario
        .add_enchantment_from_oracle(
            P0,
            "The General",
            "Exile The General: Creatures you control get +1/+1 until end of turn. Untap them.",
        )
        .id();
    let mut runner: GameRunner = scenario.build();
    runner.state_mut().objects.get_mut(&a).unwrap().tapped = true;
    runner.activate(src, 0).resolve();
    evaluate_layers(runner.state_mut());

    assert_eq!(published_set(runner.state()), ids(&[a]));
    assert!(!tapped(runner.state(), a));
    assert_eq!(runner.state().objects[&a].power, Some(3), "pumped");
}

/// Essence of Antiquity — a `GenericEffect` head (a keyword grant, not a pump)
/// feeding the same implicit-pronoun untap. This is the third publisher class in
/// the parser predicate, and the one with no `PumpAll` involved at all.
///
/// DISCLOSED DEVIATION: the printed card fires this off a Disguise
/// turn-face-up trigger, which this harness cannot drive. The body is given as a
/// `{T}` activated ability on a creature, which keeps every element the row
/// turns on — the same broadcast `affected` filter, the same implicit-pronoun
/// untap, and a real permanent source. `{T}` also taps the source, so the source
/// joining the untapped population ("creatures you control" includes it) is
/// directly observable.
#[test]
fn essence_of_antiquity_untaps_the_creatures_it_granted_hexproof() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let a = scenario.add_creature(P0, "Mine A", 2, 2).id();
    let src = scenario
        .add_creature_from_oracle(
            P0,
            "Essence Body",
            1,
            10,
            "{T}: Creatures you control gain hexproof until end of turn. Untap them.",
        )
        .id();
    let mut runner: GameRunner = scenario.build();
    runner.state_mut().objects.get_mut(&a).unwrap().tapped = true;
    runner.activate(src, 0).resolve();
    evaluate_layers(runner.state_mut());

    assert_eq!(published_set(runner.state()), ids(&[a, src]));
    assert!(!tapped(runner.state(), a));
    assert!(
        !tapped(runner.state(), src),
        "the source is one of 'creatures you control', so its own {{T}} tap is undone"
    );
    assert!(grant_lands_on(runner.state(), a, "Hexproof"));
}

/// Valley Floodcaller's cast trigger.
///
/// KNOWN, BOUNDED GAP (issue #7451): the grant's four-subtype filter
/// ("Birds, Frogs, Otters, and Rats") is misparsed upstream of this change —
/// only the last subtype survives into the pumped population. This row therefore
/// asserts the INVARIANT this PR owns, which holds regardless of that bug:
/// **the untapped set is exactly the pumped set is exactly the published set.**
/// Before the fix nothing untapped at all, so the row is strictly closer to
/// correct; when #7451 is fixed the pumped set widens and this test follows it
/// without needing to change, because it asserts the identity and not a
/// hard-coded population.
#[test]
fn valley_floodcaller_untaps_exactly_the_creatures_it_pumped() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bird = scenario
        .add_creature(P0, "Birdy", 1, 1)
        .with_subtypes(vec!["Bird"])
        .id();
    let frog = scenario
        .add_creature(P0, "Froggy", 1, 1)
        .with_subtypes(vec!["Frog"])
        .id();
    let otter = scenario
        .add_creature(P0, "Ottery", 1, 1)
        .with_subtypes(vec!["Otter"])
        .id();
    let rat = scenario
        .add_creature(P0, "Ratty", 1, 1)
        .with_subtypes(vec!["Rat"])
        .id();
    let bear = scenario.add_creature(P0, "Beary", 2, 2).id();
    scenario.add_creature_from_oracle(
        P0,
        "Valley Floodcaller",
        2,
        2,
        "Flash\nYou may cast noncreature spells as though they had flash.\nWhenever you cast a noncreature spell, Birds, Frogs, Otters, and Rats you control get +1/+1 until end of turn. Untap them.",
    );
    let bolt = scenario.add_bolt_to_hand(P0);
    let subjects = [bird, frog, otter, rat, bear];
    let mut runner: GameRunner = scenario.build();
    for id in subjects {
        runner.state_mut().objects.get_mut(&id).unwrap().tapped = true;
    }
    let base_power: Vec<Option<i32>> = subjects
        .iter()
        .map(|id| runner.state().objects[id].power)
        .collect();
    runner.cast(bolt).target_player(P1).resolve();
    runner.advance_until_stack_empty();
    evaluate_layers(runner.state_mut());

    let pumped: Vec<u64> = subjects
        .iter()
        .zip(&base_power)
        .filter(|(id, before)| runner.state().objects[*id].power != **before)
        .map(|(id, _)| id.0)
        .collect();
    let untapped: Vec<u64> = subjects
        .iter()
        .filter(|id| !tapped(runner.state(), **id))
        .map(|id| id.0)
        .collect();

    assert!(
        !pumped.is_empty(),
        "non-vacuity: the trigger must have pumped something, or the identity below is trivial"
    );
    assert_eq!(pumped, untapped, "untapped set == pumped set");
    assert_eq!(published_set(runner.state()), pumped, "== published set");
    assert!(
        !untapped.contains(&bear.0),
        "the plain creature is outside the grant's population under any reading of it"
    );
}

/// Trystan's Command — THE BOARD-VISIBLE HEADLINE for the mode-scoping fix.
/// This row was committed INVERTED, pinning the rules-wrong outcome with a doc
/// comment mandating this flip; the flip is what that comment asked for.
///
/// The card is MODAL (choose two of four sibling abilities), not a chain — the
/// engine linearizes the chosen modes into ONE resolution chain, so the publish
/// gate used to see a later mode's consumer as if it were its own. With the
/// destroy mode chosen alongside the pump mode, the destroy published first,
/// `is_sole_chain_producer`'s leg 1 declined the mass pump, and mode 4's anaphor
/// resolved against the DESTROYED creature — untapping nothing.
///
/// CR 700.2: "each of those options is a mode", i.e. a separate instruction.
/// CR 608.2c ("apply the rules of English"): "Untap them" names the creatures
/// THIS mode just pumped; a sibling mode's `Destroy` is not its antecedent.
/// `next_sub_needs_tracked_set` now stops at the mode boundary, so mode 3's
/// destroy no longer publishes for mode 4's consumer and mode 4 publishes its
/// own pumped population.
///
/// STRUCTURAL SCOPE — this is not one mode pair. MEASURED: the card is
/// `min_choices: 2, max_choices: 2` over `mode_count: 4`, so a companion mode is
/// ALWAYS chosen, and all three possible companions published before mode 4
/// resolved: destroy (this row), token copy (the row below, `[0, 3]`), and
/// graveyard-return (`[1, 3]`, measured `tracked_sets = [(1, [grave_card])]`).
/// Mode 4 was board-wrong on 3 of 3 legal pairs.
///
/// DISCRIMINATION: the two flipping values are `published_set` (`[victim]` ->
/// `[mine]`) and `tapped(mine)` (`true` -> `false`). Revert the mode-boundary
/// stop in `next_sub_needs_tracked_set` and both go back. `power == Some(5)` and
/// the graveyard assertion are the paired NON-VACUITY witnesses: they prove both
/// modes really executed, so a `!tapped` reading cannot come from the pump
/// simply never running. `published_set` panics on a second set, so it doubles
/// as a "the fix did not fragment the chain into two sets" assertion — the count
/// stays 1.
#[test]
fn trystans_command_pump_mode_untaps_the_population_its_own_mode_pumped() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let victim = scenario.add_creature(P1, "Victim", 2, 2).id();
    let mine = scenario.add_creature(P0, "Mine", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Trystan's Command",
            false,
            "Choose two —\n• Create a token that's a copy of target Elf you control.\n• Return one or two target permanent cards from your graveyard to your hand.\n• Destroy target creature or enchantment.\n• Creatures target player controls get +3/+3 until end of turn. Untap them.",
        )
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner: GameRunner = scenario.build();
    for id in [victim, mine] {
        runner.state_mut().objects.get_mut(&id).unwrap().tapped = true;
    }
    runner
        .cast(spell)
        .modes(&[2, 3])
        .target_objects(&[victim])
        .target_player(P0)
        .resolve();
    evaluate_layers(runner.state_mut());

    assert_eq!(
        published_set(runner.state()),
        ids(&[mine]),
        "CR 608.2c: mode 4's \"them\" names the creatures mode 4 pumped, not the \
         sibling mode's destroy victim"
    );
    assert_eq!(
        runner.state().objects[&victim].zone,
        engine::types::zones::Zone::Graveyard,
        "non-vacuity: the destroy mode really executed"
    );
    assert_eq!(
        runner.state().objects[&mine].power,
        Some(5),
        "non-vacuity: the pump mode really executed too"
    );
    assert!(
        !tapped(runner.state(), mine),
        "CR 701.26b: the pumped creature untaps — the defect this PR fixes"
    );
}

/// The token-copy companion mode: the same fix reached through a DIFFERENT
/// publishing arm. Also committed inverted, also flipped here.
///
/// Modes 1 and 4 (`[0, 3]`). The copy token's creation used to publish first,
/// leg 1 declined the mass pump, and mode 4's anaphor bound the TOKEN — so the
/// pumped creatures stayed tapped even though the pump itself ran. The
/// mode-boundary stop is arm-agnostic: it is not "the destroy arm was special",
/// it is that no earlier MODE publishes for a later mode's anaphor.
///
/// REDUNDANT BY MECHANISM with the row above — same crossing, same predicate.
/// Kept for card/arm coverage; do NOT cite it as a second independent bar.
#[test]
fn trystans_command_token_copy_mode_no_longer_preempts_the_pump_anaphor() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let elf = scenario
        .add_creature(P0, "Elf Pal", 1, 1)
        .with_subtypes(vec!["Elf"])
        .id();
    let mine = scenario.add_creature(P0, "Mine", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Trystan's Command",
            false,
            "Choose two —\n• Create a token that's a copy of target Elf you control.\n• Return one or two target permanent cards from your graveyard to your hand.\n• Destroy target creature or enchantment.\n• Creatures target player controls get +3/+3 until end of turn. Untap them.",
        )
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner: GameRunner = scenario.build();
    for id in [elf, mine] {
        runner.state_mut().objects.get_mut(&id).unwrap().tapped = true;
    }
    runner
        .cast(spell)
        .modes(&[0, 3])
        .target_objects(&[elf])
        .target_player(P0)
        .resolve();
    evaluate_layers(runner.state_mut());

    let token: Vec<u64> = runner
        .state()
        .battlefield
        .iter()
        .filter(|id| **id != elf && **id != mine)
        .map(|id| id.0)
        .collect();
    assert_eq!(token.len(), 1, "non-vacuity: the copy mode really ran");
    // Mode 4 pumps "creatures target player controls", and by the time it
    // resolves that is the elf, `mine`, AND the token mode 1 just created — CR
    // 611.2c fixes the affected set when the continuous effect begins, and the
    // token already exists. So the token's presence here is the PUMP's own
    // population, not a leak: pre-fix this set was the token ALONE.
    let mut expected = ids(&[elf, mine]);
    expected.extend_from_slice(&token);
    expected.sort_unstable();
    assert_eq!(
        published_set(runner.state()),
        expected,
        "CR 608.2c: mode 4's \"them\" is the population mode 4 pumped, not the \
         sibling mode's token alone"
    );
    assert_eq!(
        runner.state().objects[&mine].power,
        Some(5),
        "non-vacuity: the pump mode really executed too"
    );
    assert!(
        !tapped(runner.state(), mine),
        "CR 701.26b: the pumped creature untaps — the defect this PR fixes"
    );
}

/// Settle Beyond Reality (2X2, `{4}{W}` sorcery) — a tracked-set-CONTENTS row
/// plus a shadowing canary. It is NOT board-visible: an earlier draft claimed it
/// was, and running it falsified that (see the shadowing paragraph below). Oracle
/// text verbatim from Scryfall; both modes chosen ("Choose one or both —",
/// `min_choices: 1, max_choices: 2`).
///
/// MEASURED parse shape at `77e686cae` (probe over `parse_oracle_text`):
///   * mode 0 — `ChangeZone { destination: Exile, target: Typed { Creature,
///     controller: Opponent } }`, no sub-ability;
///   * mode 1 — `ChangeZone { destination: Exile, target: Typed { Creature,
///     controller: You } }` -> `ChangeZone { destination: Battlefield, target:
///     TrackedSet { id: 0 } }`.
///
/// `TrackedSetId(0)` is the "most recent set" sentinel: it binds to the
/// HIGHEST-id tracked set, whatever produced it. The publish gate
/// (`next_sub_needs_tracked_set`) is chain-wide, and the modes are linearized
/// into ONE resolution chain, so mode 0's exile sees mode 1's `TrackedSet`
/// consumer below it and publishes `[theirs]`. Mode 1's exile then EXTENDS that
/// same set to `[theirs, mine]`. **This row asserts the SET, not the board.**
///
/// MEASURED at `77e686cae`: the board is already correct here, and an earlier
/// draft of this comment claiming otherwise was falsified by running it. A
/// singular `ChangeZone` calls `targeting::resolved_targets` then
/// `effects::effect_object_targets`, where `TargetFilter::TrackedSet` falls into
/// the `_ =>` arm that returns `ability.targets` — this mode's OWN inherited
/// chosen target, `[mine]`. `targeted_objects` is then non-empty, so the
/// untargeted zone-scan path (which would have used `matches_target_filter` and
/// moved every set member) is never reached. **Chosen-target inheritance is
/// already mode-scoped and SHADOWS the sentinel for this consumer**, which is
/// why the leaked `[theirs, mine]` is inert on the board here.
///
/// So this is a tracked-set-CONTENTS row. The two "theirs stays exiled"
/// assertions below PASS pre-fix: they are a no-regression **canary** that would
/// flip if anyone ever unshadowed `ChangeZone`, NOT discriminators. Only the
/// `published_set` assertion discriminates. Any later claim that the zone
/// assertions discriminate is a re-justification, not a restatement.
///
/// CR 700.2 + CR 700.2a: each bullet is a separate mode, chosen independently.
/// CR 608.2c ("apply the rules of English"): "it" in mode 1 is a
/// nearest-antecedent pronoun bound to mode 1's own exile — a sibling mode's
/// exile is not its antecedent. The opponent's creature must stay in exile.
///
/// The three assertions are load-bearing in this order:
///  1. non-vacuity — mode 0's exile really executed (a `ZoneChanged` to Exile for
///     the opponent's creature). Without it, "the opponent's creature is not on
///     the battlefield" would also pass if mode 0 had silently never run, and
///     "it is on the battlefield" could not be distinguished from "it was never
///     exiled";
///  2. THE DEFECT — the opponent's creature is still in exile;
///  3. the published set contains only mode 1's own exile. `published_set`
///     panics on a second set, so it doubles as a "the fix did not fragment the
///     chain" assertion.
#[test]
fn settle_beyond_reality_return_mode_returns_only_the_creature_its_own_mode_exiled() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let theirs = scenario.add_creature(P1, "Their Bear", 2, 2).id();
    let mine = scenario.add_creature(P0, "My Bear", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Settle Beyond Reality",
            false,
            "Choose one or both —\n• Exile target creature you don't control.\n• Exile target creature you control, then return it to the battlefield under its owner's control.",
        )
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner: GameRunner = scenario.build();
    let outcome = runner
        .cast(spell)
        .modes(&[0, 1])
        .target_objects(&[theirs, mine])
        .resolve();

    let exiled_theirs = outcome
        .events()
        .iter()
        .filter(|e| {
            matches!(
                e,
                engine::types::events::GameEvent::ZoneChanged {
                    object_id,
                    to: engine::types::zones::Zone::Exile,
                    ..
                } if *object_id == theirs
            )
        })
        .count();
    assert_eq!(
        exiled_theirs, 1,
        "non-vacuity: mode 0 must actually exile the opponent's creature, \
         otherwise the exile assertion below is vacuous"
    );

    assert_eq!(
        runner.state().objects[&theirs].zone,
        engine::types::zones::Zone::Exile,
        "CR 608.2c: mode 1's \"return it\" names the creature MODE 1 exiled; \
         the opponent's creature must stay exiled"
    );
    assert!(
        !runner.state().battlefield.contains(&theirs),
        "CR 700.2: a sibling mode's exile is not mode 1's antecedent — the \
         opponent's creature must not come back"
    );

    assert_eq!(
        runner.state().objects[&mine].zone,
        engine::types::zones::Zone::Battlefield,
        "CR 400.7j: an effect that moves an object to a public zone can still \
         find it, so mode 1 returns its own exiled creature to the battlefield"
    );
    assert!(
        runner.state().battlefield.contains(&mine),
        "mode 1's own creature is the whole population of \"return it\""
    );

    assert_eq!(
        published_set(runner.state()),
        ids(&[mine]),
        "CR 608.2c: the tracked set mode 1's anaphor binds holds only mode 1's \
         own exile"
    );
}

/// SYNTHESIZED, disclosed (precedent: this file's "Bare Pump" rows) — the
/// discriminator for the SECOND mode-boundary crossing: the recursive descents
/// inside `ability_or_branch_references_tracked_set`.
///
/// Guarding only the ENTRY HOP in `next_sub_needs_tracked_set` is insufficient
/// whenever a mode has MORE THAN ONE node, because `append_to_sub_chain` hangs
/// the next mode's root off the TAIL of the current mode's own sub-chain. Here
/// mode 1 is `Destroy -> Draw`, so the entry hop lands on the `Draw` — which
/// carries no ordinal (it is within-mode) and does not consume, so it passes —
/// and the recursion below it reaches mode 2's root. Without the stop on that
/// recursion, mode 1's `Destroy` publishes `[victim]` for mode 2's "Untap
/// them", `is_sole_chain_producer`'s leg 1 then declines mode 2's own publish,
/// and the untap binds a creature that is already in the graveyard.
///
/// DISCRIMINATION: delete the stop from the `sub_ability` descent in
/// `ability_or_branch_references_tracked_set` and `published_set` goes back to
/// `[victim]` while `tapped(mine)` goes back to `true`. The single-node modes on
/// the real cards above cannot flip this — their entry hop already lands on the
/// next mode's root, so crossing #1 alone covers them.
///
/// The `else_ability` descent of the same function has NO discriminating row and
/// no coverage is claimed for it: reverting it (together with crossing #4, the
/// other `else_ability` guard) leaves the integration suite at 5131 passed / 0
/// failed. It cannot currently be reached — `append_to_sub_chain` walks only
/// `sub_ability`, and `build_chained_resolved` is the sole writer of
/// `modal_instruction_ordinal`, so no mode root can sit in an `else_ability`
/// slot. It is kept for uniformity across the descents, not for a demonstrated
/// defect.
///
/// WHY SYNTHESIZED: the plan named Grub's Command as the real carrier. MEASURED
/// against `parse_oracle_text` in this worktree, it is not one — its pump bullet
/// lowers to a SINGLE `GenericEffect` node with no rider, and its mill bullet's
/// consumer lowers to a singular `ChangeZone { destination: Hand, target:
/// TrackedSetFiltered { id: 0, filter: Any } }` whose Goblin restriction the
/// parser drops and which is inert at runtime. It could not have discriminated
/// anything. The two bullets glued here are the verbatim shapes this file
/// already exercises separately.
///
/// CR 700.2 (each bullet is a mode) + CR 608.2c ("Untap them" names the
/// creatures its OWN mode pumped) + CR 701.26b (untap).
#[test]
fn modal_two_node_mode_does_not_publish_for_a_later_modes_anaphor() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let victim = scenario.add_creature(P1, "Victim", 2, 2).id();
    let mine = scenario.add_creature(P0, "Mine", 2, 2).id();
    scenario.with_library_top(P0, &["Lib A"]);
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Two Node Command",
            false,
            "Choose two —\n• Destroy target creature. Draw a card.\n• Creatures target player controls get +3/+3 until end of turn. Untap them.",
        )
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner: GameRunner = scenario.build();
    let hand_before = runner.state().players[0].hand.len();
    for id in [victim, mine] {
        runner.state_mut().objects.get_mut(&id).unwrap().tapped = true;
    }

    runner
        .cast(spell)
        .modes(&[0, 1])
        .target_objects(&[victim])
        .target_player(P0)
        .resolve();
    evaluate_layers(runner.state_mut());

    assert_eq!(
        runner.state().objects[&victim].zone,
        engine::types::zones::Zone::Graveyard,
        "non-vacuity: mode 1's destroy really executed, so it really was in \
         publisher position when the descent ran"
    );
    assert_eq!(
        runner.state().players[0].hand.len(),
        hand_before,
        "non-vacuity: mode 1's SECOND node really executed too (the cast spent \
         the spell from hand and the draw replaced it), so the entry hop really \
         landed on a within-mode node rather than on mode 2's root"
    );
    assert_eq!(
        runner.state().objects[&mine].power,
        Some(5),
        "non-vacuity: mode 2's pump really executed"
    );
    assert_eq!(
        published_set(runner.state()),
        ids(&[mine]),
        "CR 608.2c: mode 2's \"them\" is mode 2's own pumped population — mode \
         1's destroy must not have published across the boundary"
    );
    assert!(
        !tapped(runner.state(), mine),
        "CR 701.26b: the pumped creature untaps"
    );
}

/// Expose the Culprit, modes `[0, 1]` — POSITIVE CONTROL for the class of
/// producers that already allocate a fresh, strictly-greater set id.
///
/// NO-REGRESSION. Mode 2 (index 1) heads with `ChooseObjectsIntoTrackedSet`,
/// which publishes through `publish_fresh_tracked_set` — an UNGATED site that
/// never consults `next_sub_needs_tracked_set` at all. The mode-boundary work
/// must leave it exactly where it was: the pile it cloaks is the pile the player
/// chose, no more and no less, with mode 1's unrelated turn-face-up in front of
/// it in the same resolution chain.
///
/// CR 701.58a (cloak) + CR 700.2 (modes are separate instructions).
#[test]
fn expose_the_culprit_cloaks_only_the_pile_it_chose() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let hidden = scenario.add_creature(P0, "Hidden", 2, 2).id();
    let pile: Vec<ObjectId> = ["Dis A", "Dis B"]
        .iter()
        .map(|n| {
            scenario
                .add_creature(P0, n, 2, 2)
                .with_keyword(engine::types::keywords::Keyword::Disguise(
                    ManaCost::generic(3).into(),
                ))
                .id()
        })
        .collect();
    let bystander = scenario
        .add_creature(P0, "Dis C", 2, 2)
        .with_keyword(engine::types::keywords::Keyword::Disguise(
            ManaCost::generic(3).into(),
        ))
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Expose the Culprit",
            false,
            "Choose one or both —\n• Turn target face-down creature face up.\n• Exile any number of face-up creatures you control with disguise in a face-down pile, shuffle that pile, then cloak them.",
        )
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner: GameRunner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&hidden)
        .unwrap()
        .face_down = true;

    runner
        .cast(spell)
        .modes(&[0, 1])
        .target_objects(&[hidden])
        .commit();
    runner.advance_until_stack_empty();
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseObjectsSelection { .. }
        ),
        "reach-guard: mode 2's pile selection must be reached, or nothing below \
         is about the fresh-publish class. got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SelectTargets {
            targets: pile.iter().map(|&id| TargetRef::Object(id)).collect(),
        })
        .expect("pile selection accepted");

    assert!(
        !runner.state().objects[&hidden].face_down,
        "non-vacuity: mode 1 really turned its own target face up, so both modes \
         ran in one chain"
    );
    for &id in &pile {
        assert!(
            runner.state().objects[&id].face_down,
            "CR 701.58a: every chosen creature is cloaked"
        );
    }
    assert!(
        !runner.state().objects[&bystander].face_down,
        "an unchosen disguise creature is outside the pile under any reading"
    );
    // `ChooseObjectsIntoTrackedSet` publishes a fresh EMPTY set at its head and
    // then the chosen pile as a strictly-greater id, so this chain legitimately
    // holds two sets and `published_set`'s single-set helper does not apply.
    let sets = tracked_sets(runner.state());
    assert_eq!(
        sets.last().map(Vec::as_slice),
        Some(ids(&pile).as_slice()),
        "the fresh-published pile is the highest-id set, unchanged by the \
         mode-boundary work: got {sets:?}"
    );
}

/// Drive the game forward until every delayed trigger has resolved, answering
/// the turn-based actions that would otherwise stall a bare priority pass.
fn advance_until_delayed_triggers_resolve(runner: &mut GameRunner) {
    for guard in 0..256 {
        if runner.state().delayed_triggers.is_empty() && runner.state().stack.is_empty() {
            return;
        }
        let action = match &runner.state().waiting_for {
            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            },
            WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                assignments: vec![],
            },
            WaitingFor::DiscardToHandSize { count, cards, .. } => GameAction::SelectCards {
                cards: cards.iter().take(*count).copied().collect(),
            },
            _ => GameAction::PassPriority,
        };
        assert!(
            runner.act(action).is_ok(),
            "stalled at guard {guard}: phase={:?} waiting={:?}",
            runner.state().phase,
            runner.state().waiting_for
        );
    }
    panic!("delayed trigger never resolved");
}

// ===========================================================================
// CROSS-`SequentialSibling` NO-REGRESSION GATES
//
// Both cards are NON-MODAL, so `modal_instruction_ordinal` is `None` on every
// node and the mode-boundary machinery is provably inert for them. That is the
// point: they hold the mode-keyed design to its claim that a sentence boundary
// is not a mode boundary.
//
// WHICH OF THE TWO ACTUALLY GUARDS THAT — MEASURED, not derived. Probe: key the
// reset on `sub_link == SubAbilityLink::SequentialSibling` instead of the mode
// ordinal, run the whole integration suite (8 rows red of 5131):
//
//   * RANDOM ENCOUNTER goes RED — it is the mis-key guard. Its chain fragments
//     into two tracked sets (`[[1, 1, 2, 2, 3, 4], []]`) and the single-set
//     precondition in `published_set` fails. NOTE this is NOT its delayed
//     `Bounce`, which is a separately measured runtime no-op: the guard value is
//     the fragmentation, not the bounce.
//   * EPIC EXPERIMENT stays GREEN — so despite having the deeper cross-sibling
//     consumer, it does NOT discriminate a `sub_link` mis-key and no guard status
//     is claimed for it. It is derivation-only here.
//
// Two successive readings of these rows (mine, then a reviewer's) predicted the
// opposite pairing. Both were wrong. Do not re-derive this from the chain shapes
// below — re-run the probe.
//
// Chains dumped from `parse_oracle_text` in this worktree (not inferred from
// the Oracle text's punctuation — that inference has been measured wrong here):
//
//   Epic Experiment:   ExileTop(ContinuationStep)
//                        -> CastFromZone(SequentialSibling)
//                          -> ChangeZoneAll{TrackedSetFiltered{0}}(SequentialSibling)
//
//   Random Encounter:  Shuffle(ContinuationStep)
//                        -> Mill(ContinuationStep)
//                          -> ChangeZoneAll{TrackedSetFiltered{0,Creature}}(ContinuationStep)
//                            -> haste GenericEffect(SequentialSibling)
//                              -> CreateDelayedTrigger{Bounce{TrackedSet{0}}}(SequentialSibling)
// ===========================================================================

/// Epic Experiment — a tracked-set consumer TWO `SequentialSibling` hops below
/// its producer, with an INTERACTIVE cast step inside the window.
///
/// The `ExileTop` publishes; `CastFromZone` (a `SequentialSibling`, and a real
/// pause) sits between; and `ChangeZoneAll { TrackedSetFiltered { 0 } }` — "put
/// all cards exiled this way that weren't cast into your graveyard" — is a
/// second `SequentialSibling` below that.
///
/// NO GUARD STATUS IS CLAIMED. The obvious derivation — "a reset keyed on
/// `sub_link` would fire twice inside one instruction and orphan the anaphor, so
/// the exiled cards would stay in exile forever" — was MEASURED FALSE: under that
/// exact mis-key this row stays GREEN (see the section header). WHY it survives
/// is not measured — the standing candidate, that `TrackedSetFiltered(0)` binds
/// through `targeting::resolve_tracked_set_id` whose later rungs still find the
/// `ExileTop` set after a chain-id clear, is a reading and is recorded as one.
/// Keep this as a derivation-only no-regression row; the measured mis-key
/// witness is Random Encounter.
///
/// CR 608.2c: this is ONE instruction sequence, not two. The card is not modal,
/// so `crosses_modal_boundary` is false at every node and the binding must
/// survive byte-identically.
#[test]
fn epic_experiment_binds_its_exiled_set_across_two_sequential_siblings() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Mana value 5 each, so `X = 2` makes NONE of them castable: the
    // `CastFromZone` step is reached and offers nothing, and every exiled card
    // must fall through to the graveyard step below it.
    let lib: Vec<ObjectId> = ["Lib A", "Lib B"]
        .iter()
        .map(|n| {
            scenario
                .add_spell_to_library_top(P0, n, false)
                .with_mana_cost(ManaCost::generic(5))
                .from_oracle_text("You gain 1 life.")
                .id()
        })
        .collect();
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Epic Experiment",
            false,
            "Exile the top X cards of your library. You may cast instant and sorcery spells with mana value X or less from among them without paying their mana costs. Then put all cards exiled this way that weren't cast into your graveyard.",
        )
        .with_mana_cost(ManaCost::Cost {
            shards: vec![engine::types::mana::ManaCostShard::X],
            generic: 0,
        })
        .id();
    scenario.with_mana_pool(
        P0,
        (0..2)
            .map(|_| {
                engine::types::mana::ManaUnit::new(
                    engine::types::mana::ManaType::Colorless,
                    ObjectId(0),
                    false,
                    vec![],
                )
            })
            .collect(),
    );
    let mut runner: GameRunner = scenario.build();
    runner.cast(spell).x(2).resolve();

    assert_eq!(
        published_set(runner.state()),
        ids(&lib),
        "CR 608.2c: the exiled cards remain the anaphor's antecedent across both \
         SequentialSibling hops"
    );
    for &id in &lib {
        assert_eq!(
            runner.state().objects[&id].zone,
            engine::types::zones::Zone::Graveyard,
            "the uncast exiled cards reach the graveyard — the observable that \
             an orphaned anaphor would leave stranded in exile"
        );
    }
}

/// Random Encounter — the harder cross-`SequentialSibling` case: a DELAYED
/// consumer, two `SequentialSibling` hops deep, that reads slot 0 at the next
/// end step rather than during the resolution that published it.
///
/// `Mill` publishes; `ChangeZoneAll { TrackedSetFiltered { 0, Creature } }` puts
/// the milled creatures onto the battlefield; then, past a haste rider and a
/// `CreateDelayedTrigger`, both `SequentialSibling`, the delayed "return those
/// creatures to their owner's hand" binds `TrackedSet { 0 }`.
///
/// THIS ROW IS THE MIS-KEY GUARD, measured (see the section header): with the
/// reset keyed on `sub_link` instead of the mode ordinal, this chain fragments
/// into two tracked sets (`[[1, 1, 2, 2, 3, 4], []]`) and the single-set
/// precondition in `published_set` fails. The guard value is the FRAGMENTATION,
/// not the delayed bounce — the bounce is a separately measured runtime no-op
/// (see the KNOWN GAP note at the end of this test), so no claim rides on it.
///
/// CR 603.7 (delayed triggered ability) + CR 608.2c. Non-modal, so the
/// mode-boundary machinery is inert here by construction.
#[test]
fn random_encounter_delayed_bounce_binds_across_two_sequential_siblings() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let lib: Vec<ObjectId> = ["Lib A", "Lib B", "Lib C", "Lib D"]
        .iter()
        .map(|n| scenario.add_card_to_library_top(P0, n))
        .collect();
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Random Encounter",
            false,
            "Shuffle your library, then mill four cards. Put each creature card milled this way onto the battlefield. They gain haste. At the beginning of the next end step, return those creatures to their owner's hand.",
        )
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner: GameRunner = scenario.build();
    // Two of the four milled cards are creatures, so the reanimated population
    // is a proper subset of the milled set.
    let creatures = [lib[0], lib[1]];
    for &id in &creatures {
        let obj = runner.state_mut().objects.get_mut(&id).unwrap();
        obj.card_types.core_types = vec![CoreType::Creature];
        obj.base_card_types.core_types = vec![CoreType::Creature];
        obj.power = Some(2);
        obj.toughness = Some(2);
        obj.base_power = Some(2);
        obj.base_toughness = Some(2);
    }
    runner.cast(spell).resolve();

    for &id in &creatures {
        assert_eq!(
            runner.state().objects[&id].zone,
            engine::types::zones::Zone::Battlefield,
            "non-vacuity: the milled creatures really entered, or the delayed \
             bounce below has nothing to bind"
        );
    }
    assert!(
        !runner.state().delayed_triggers.is_empty(),
        "reach-guard: the delayed 'return those creatures' trigger was created"
    );

    let set = published_set(runner.state());
    for &id in &creatures {
        assert!(
            set.contains(&id.0),
            "CR 603.7 + CR 608.2c: the set the delayed trigger will read still \
             holds the creatures this resolution put onto the battlefield, two \
             SequentialSibling hops above it. got {set:?}"
        );
    }

    advance_until_delayed_triggers_resolve(&mut runner);

    // KNOWN GAP, MEASURED at this tree and NOT caused by this PR: the end-step
    // return is a no-op. The delayed ability is created carrying an UNBOUND
    // `Bounce { target: TrackedSet { id: 0 } }` with an empty `targets` list
    // (dumped), and draining it to the end step produces ZERO `ZoneChanged`
    // events. Every node of this non-modal card carries
    // `modal_instruction_ordinal: None` (also dumped), so all four mode-boundary
    // crossings are inert here by construction and this reading is byte-identical
    // to the pre-PR engine.
    //
    // Pinned rather than omitted, in this file's established canary style: when
    // the delayed `TrackedSet(0)` binding is fixed, these two assertions go RED
    // and must be flipped to `Zone::Hand`, not deleted.
    for &id in &creatures {
        assert_eq!(
            runner.state().objects[&id].zone,
            engine::types::zones::Zone::Battlefield,
            "unchanged from before this PR — see the KNOWN GAP note above"
        );
    }
}

/// SYNTHESIZED, disclosed (precedent: this file's "Bare Pump" rows) — the ONLY
/// row that reverts on the boundary RESET alone, and therefore the only evidence
/// that the reset earns its place beside the four crossings.
///
/// Both bullets are verbatim shapes this file/PR already exercise separately:
/// bullet 1 is Settle Beyond Reality's return mode, bullet 2 is Trystan's
/// Command mode 4. Glued into one card, they produce the shape the crossings
/// CANNOT reach: mode 1 publishes for its OWN within-mode consumer, so the
/// mode-boundary stop in `next_sub_needs_tracked_set` correctly does NOT fire —
/// that publish is legitimate.
///
/// What happens next is the defect the reset closes. Mode 1 leaves
/// `chain_tracked_set_id` pointing at a NON-EMPTY set. Mode 2's `PumpAll` then
/// consults `is_sole_chain_producer`, whose leg 1 is
/// `chain_tracked_set_id.is_none_or(|id| set.is_empty())` — false — so the pump
/// declines to publish, falls through to the `_ =>` `ZoneChanged` harvest (empty
/// for an event-less producer), and `publish_tracked_set([])` EXTENDS mode 1's
/// set instead of allocating a new one. "Untap them" then binds mode 1's
/// flickered creature and the pumped creature stays tapped.
///
/// The reset makes leg 1 true at every mode root by construction.
///
/// DISCRIMINATION: `tapped(mine)` `true` -> `false`, and `tracked_sets().len()`
/// `1` -> `2`. Revert the reset in `resolve_ability_chain` and both go back;
/// reverting the four crossings instead leaves this row GREEN, which is what
/// makes it commit-specific rather than a duplicate of the rows above.
///
/// CR 700.2 + CR 608.2c + CR 701.26b.
#[test]
fn modal_pump_mode_untaps_its_own_population_when_an_earlier_mode_published_for_itself() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let flickered = scenario.add_creature(P0, "Flickered", 2, 2).id();
    let mine = scenario.add_creature(P0, "Mine", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Self Publishing Command",
            false,
            "Choose one or both —\n• Exile target creature you control, then return it to the battlefield under its owner's control.\n• Creatures target player controls get +3/+3 until end of turn. Untap them.",
        )
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner: GameRunner = scenario.build();
    runner.state_mut().objects.get_mut(&mine).unwrap().tapped = true;

    runner
        .cast(spell)
        .modes(&[0, 1])
        .target_objects(&[flickered])
        .target_player(P0)
        .resolve();
    evaluate_layers(runner.state_mut());

    assert_eq!(
        runner.state().objects[&flickered].zone,
        engine::types::zones::Zone::Battlefield,
        "non-vacuity: mode 1's exile-and-return really executed, so it really \
         published a non-empty set of its own"
    );
    assert_eq!(
        runner.state().objects[&mine].power,
        Some(5),
        "non-vacuity: mode 2's pump really executed"
    );
    let sets = tracked_sets(runner.state());
    assert_eq!(
        sets.len(),
        2,
        "CR 700.2: two modes, two instructions, two sets — mode 2 must ALLOCATE \
         rather than extend mode 1's. got {sets:?}"
    );
    // Mode 2 pumps "creatures target player controls", and the flickered
    // creature is back on the battlefield by then (CR 611.2c fixes the affected
    // set when the continuous effect begins), so BOTH are mode 2's own
    // population. Pre-reset there is only ONE set and it holds `[flickered]`
    // alone — mode 2's harvest was empty and merely extended mode 1's set.
    assert_eq!(
        sets.last().map(Vec::as_slice),
        Some(ids(&[flickered, mine]).as_slice()),
        "CR 608.2c: mode 2's own pumped population is the highest-id set"
    );
    assert!(
        !tapped(runner.state(), mine),
        "CR 701.26b: the pumped creature untaps"
    );
}

/// ANTI-VACUITY INSTRUMENT for the row above, and a no-regression row in its own
/// right: the SAME synthesized card with only its pump bullet chosen.
///
/// If this reads RED, the pump bullet does not lower to `PumpAll ->
/// SetTapState { target: TrackedSet }` at all, the row above is vacuous in BOTH
/// directions, and its verdict is void. It must pass before AND after the reset.
#[test]
fn modal_pump_mode_untaps_when_chosen_alone() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature(P0, "Flickered", 2, 2);
    let mine = scenario.add_creature(P0, "Mine", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Self Publishing Command",
            false,
            "Choose one or both —\n• Exile target creature you control, then return it to the battlefield under its owner's control.\n• Creatures target player controls get +3/+3 until end of turn. Untap them.",
        )
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner: GameRunner = scenario.build();
    runner.state_mut().objects.get_mut(&mine).unwrap().tapped = true;

    runner.cast(spell).modes(&[1]).target_player(P0).resolve();
    evaluate_layers(runner.state_mut());

    assert_eq!(
        runner.state().objects[&mine].power,
        Some(5),
        "non-vacuity: the pump really executed"
    );
    assert!(
        !tapped(runner.state(), mine),
        "the pump bullet really does lower to a TrackedSet untap — unchanged \
         before and after the reset"
    );
}

/// THE ARM-D GATE, and the discriminator for the THIRD mode-boundary crossing —
/// the stop inside `later_node_is_publisher_position`'s walk.
///
/// SYNTHESIZED, disclosed (precedent: this file's "Bare Pump" rows). Both
/// bullets are verbatim shapes this suite already exercises: bullet 1 is
/// Trystan's Command mode 4, bullet 2 is Settle Beyond Reality's return mode.
///
/// WHY NOT PLUNGE INTO DARKNESS, the plan's named carrier: MEASURED — its first
/// mode heads with `Sacrifice`, and `is_sole_chain_producer` (the function
/// crossing #3 lives in) is consulted from exactly three match guards, all of
/// them event-less producers: `Effect::PumpAll`, `Effect::GoadAll`,
/// `Effect::GiveControl`. An event-emitting producer never reaches it, so Plunge
/// could not have discriminated this crossing on any mode pair. The shape that
/// CAN is "an event-less producer with its own within-mode consumer, followed by
/// another publishing mode", which is what this card is.
///
/// THE DEFECT: `is_sole_chain_producer`'s leg 2 asks "is any LATER node in this
/// chain itself in publisher position?" and answers by walking the whole linear
/// chain. Mode 1 publishes for its own "Untap them" — legitimate, so crossing #1
/// does not fire. But the walk then continues past that untap into MODE 2's root,
/// finds mode 2's own `TrackedSet` consumer, and reports "a later publisher
/// exists" — so mode 1's `PumpAll` DECLINES to publish, its harvest is empty, and
/// its own untap binds an empty set. A later mode's producer is a different
/// instruction (CR 700.2), not a competing antecedent (CR 608.2c), so leg 2 must
/// stop at the boundary.
///
/// DISCRIMINATION, MEASURED at tip by deleting the `crosses_modal_boundary`
/// early return from `later_node_is_publisher_position::walk`: the first
/// published set goes `[mine, flickered]` -> `[]`; with that assertion relaxed
/// to a print, the untap assertion then fails too, i.e. `tapped(mine)` goes back
/// to `true`. The COUNT does not flip — both readings publish two sets — which
/// is why this row asserts contents and not `sets.len()` alone.
///
/// ARM D: two modes both publishing, each consumer resolving inside its own
/// mode. RED here means the ordering argument in `publish_tracked_set`'s doc is
/// unsound, not merely that this crossing regressed.
///
/// NOT COMMIT-EXCLUSIVE: this row also reverts on the mode-boundary reset in the
/// following commit (measured — reverting the reset reddens both this row and
/// `modal_pump_mode_untaps_its_own_population_when_an_earlier_mode_published_for_itself`),
/// so a red here localises the defect to "one of the two", not to this crossing.
/// The crossing-#3 attribution is the probe recorded above, not this row alone.
///
/// CR 700.2 + CR 608.2c + CR 701.26b.
#[test]
fn two_publishing_modes_each_bind_their_own_population() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mine = scenario.add_creature(P0, "Mine", 2, 2).id();
    let flickered = scenario.add_creature(P0, "Flickered", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Two Publisher Command",
            false,
            "Choose one or both —\n• Creatures target player controls get +3/+3 until end of turn. Untap them.\n• Exile target creature you control, then return it to the battlefield under its owner's control.",
        )
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner: GameRunner = scenario.build();
    for id in [mine, flickered] {
        runner.state_mut().objects.get_mut(&id).unwrap().tapped = true;
    }

    runner
        .cast(spell)
        .modes(&[0, 1])
        .target_player(P0)
        .target_objects(&[flickered])
        .resolve();
    evaluate_layers(runner.state_mut());

    assert_eq!(
        runner.state().objects[&mine].power,
        Some(5),
        "non-vacuity: mode 1's pump really executed, so its publish decision was \
         really taken"
    );
    assert_eq!(
        runner.state().objects[&flickered].zone,
        engine::types::zones::Zone::Battlefield,
        "non-vacuity: mode 2's exile-and-return really executed — that is what \
         puts mode 2 in PUBLISHER POSITION behind mode 1, which is the whole \
         precondition for leg 2 to have anything to find"
    );

    let sets = tracked_sets(runner.state());
    assert_eq!(
        sets.len(),
        2,
        "CR 700.2: two publishing modes, two instructions, two sets. got {sets:?}"
    );
    assert_eq!(
        sets.first().map(Vec::as_slice),
        Some(ids(&[mine, flickered]).as_slice()),
        "CR 608.2c: mode 1's set holds the population MODE 1 pumped. Empty here \
         means leg 2 walked into mode 2 and declined mode 1's publish"
    );
    assert!(
        !tapped(runner.state(), mine),
        "CR 701.26b: mode 1's own \"Untap them\" binds mode 1's own population"
    );
}
